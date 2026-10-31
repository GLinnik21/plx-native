//! **Settings > Libraries > Under posters**: what the focused poster shows beneath it.
//!
//! One global setting, saved per profile (`plx_plex::plex::session::UnderPosters`). The page holds
//! four rows: a Title switch, a Details row that opens [`DetailsPage`] (its pushed picker, one
//! checked choice per details line, each with a sample of what it shows), a Sort value
//! switch and Reset. Both pages carry
//! a framed preview: a still row of three posters from the library the Library last showed, drawn by
//! the shelves' own tile painters ([`plx_ui::cards::SampleRow`]), the middle one lifted and captioned
//! by the SAME policy the Library, Collections, the Watchlist, Home and
//! Recommended read ([`crate::registry::captions`], [`plx_ui::caption_policy`]); only that tile
//! wears a caption, and a face with no artwork carries no word. A line the setting turns on is
//! never drawn empty, for any kind of tile: a details kind a tile has no facts for (a season has no
//! length, often no genre) says the tile's identity instead ("Season 6", "S1 · E8", its year, and
//! for a film, show or collection with not even that, what it is: "Movie", "Show", "Collection").
//! Except a movie or TV show library grid showing its sort value (one value line under every card),
//! the policy is the same under every POSTER in the app (Search results, a person's filmography,
//! Detail's Related and collection shelves and a collection's members included); a landscape still
//! or a headshot is not a poster and wears none of it. The preview is built from the value
//! the rows show, so a change moves it in the frame the row changes; the picker's preview follows
//! the focused choice. With Sort value On, the value lines (on every tile, as the Movies and TV Shows grids
//! wear them, illustrating a library sorted by Year: each sample's own year, the lifted one bold)
//! preview only while the Sort value row has focus, under a "Sorted by Year" caption at the frame's
//! foot, and never in the picker: Title and Details still govern every other poster surface, so
//! their preview stays the page's default. The frame is the board's (660 x 556, Library cards
//! 35 px apart so the outer two are clipped by it and shown at 35%, their value lines with them);
//! the picker's frame says "The library you last opened" at its foot, and its Video choice carries
//! the one detail line ("A TV show has no single video quality, so it shows its year.").
//!
//! The write goes through the one door every live setting uses (`PreferenceCmd::Choose`, Confirm):
//! the row shows the pick at once, the stored value settles it when the receipt answers, and a
//! refused write (nobody seated) puts the row back.

use super::*;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_ui::caption_policy::{band_h, CaptionPolicy, DetailsKind};
use plx_data::pms::PmsMovie;
use plx_ui::caption_policy::CaptionFacts;
use plx_ui::cards::{RowStyle, SampleFrame, SampleRow, TileLabel};
use plx_ui::widgets::Art;
use crate::registry::tile_facts;

/// The stored setting as the UI's caption policy. The mapping lives here, not in `plx_plex`: the
/// data layer must not name the UI layer.
pub fn caption_policy_of(v: UnderPosters) -> CaptionPolicy {
    CaptionPolicy {
        title: v.title,
        details: match v.details {
            UnderPostersDetails::YearLength => DetailsKind::YearLength,
            UnderPostersDetails::Genre => DetailsKind::Genre,
            UnderPostersDetails::Video => DetailsKind::Video,
            UnderPostersDetails::AgeRatingScore => DetailsKind::AgeRatingScore,
            UnderPostersDetails::None => DetailsKind::None,
        },
    }
}

/// A details line's name in the picker.
fn details_label(d: UnderPostersDetails) -> &'static str {
    use plx_platform::i18n::msg;
    match d {
        UnderPostersDetails::YearLength => msg::under_posters_year_length(),
        UnderPostersDetails::Genre => msg::under_posters_genre(),
        UnderPostersDetails::Video => msg::under_posters_video(),
        UnderPostersDetails::AgeRatingScore => msg::under_posters_age_rating_score(),
        UnderPostersDetails::None => msg::under_posters_none(),
    }
}

/// **The Settings root's value for the Under posters row**: the details line the page's Details row
/// reads ("Year · Length", "Genre"), the way Language reads its language. The Title switch adds a
/// bold line above it and is the page's own row; it shows here only when it is all there is ("Title"),
/// so the value is never "Nothing" beside a poster that wears its name. Kept to the details label's
/// width because the row's sub-line shares the label column with the value, and a longer value
/// (title and details together) put an ellipsis on the sub-line in several languages.
pub(super) fn summary(v: UnderPosters) -> String {
    use plx_platform::i18n::msg;
    match (v.details, v.title) {
        (UnderPostersDetails::None, true) => msg::under_posters_title_row().to_string(),
        (details, _) => details_label(details).to_string(),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpId {
    Title,
    Details,
    SortValue,
    Reset,
}

impl FormId for UpId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            UpId::Title => 1,
            UpId::Details => 2,
            UpId::SortValue => 3,
            UpId::Reset => 30,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpAction {
    Title,
    /// The Details row is a push (`RowKind::Nav`): the form answers it with the page, never with this.
    Details,
    SortValue,
    Reset,
}

/// The page's rows from plain inputs (the text-fit tests call this directly).
fn under_posters_form(v: UnderPosters) -> Form<UpId, UpAction, SettingsPage> {
    use plx_platform::i18n::msg;
    let rows = FormSection::new("")
        .item(
            UpId::Title,
            RowKind::Toggle,
            UpAction::Title,
            Row::new(msg::under_posters_title_row()).detail(msg::under_posters_title_detail()).toggle(v.title),
        )
        .item(
            UpId::Details,
            RowKind::Nav(SettingsPage::UnderPostersDetails),
            UpAction::Details,
            Row::new(msg::under_posters_details_row())
                .detail(msg::under_posters_details_detail())
                .value(details_label(v.details))
                .chevron(true),
        );
    let rows = rows.item(UpId::SortValue, RowKind::Toggle, UpAction::SortValue,
        Row::new(msg::under_posters_sort_value()).detail(msg::under_posters_sort_value_detail()).toggle(v.sort_value));
    let reset = FormSection::new("").item(
        UpId::Reset,
        RowKind::Button,
        UpAction::Reset,
        Row::new(msg::under_posters_reset()).detail(msg::under_posters_reset_detail()).dim(v == UnderPosters::DEFAULT),
    );
    Form::new().section(rows).section(reset)
}

pub(super) struct UnderPostersPage {
    entry: EntryId,
    form: FormTable<UpId, UpAction, SettingsPage>,
    state: UpState,
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
            "under_posters sel={} title={} details={} busy={} sort_value={}",
            self.sel.0, self.value.title, self.value.details.index(), self.busy, self.value.sort_value
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
            pending: None,
        };
        page.rebuild();
        page
    }

    /// Re-derive the rows from the shown value, keeping the cursor on its row by identity.
    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(UpId::Title);
        self.form.table.compact = false;
        self.form.set(under_posters_form(self.state.value), Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        use plx_platform::i18n::msg;
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(msg::settings_title()), msg::under_posters_title(), msg::under_posters_copy()),
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

    /// The value the preview words: the rows' own, except that the sort captions (which replace the
    /// Title and Details words under every poster) show only while the Sort row has focus. Title
    /// and Details still govern Home, Search, the Watchlist and Collections, so the preview of their
    /// rows stays the caption they set.
    fn previewed(&self) -> UnderPosters {
        let v = self.state.value;
        UnderPosters { sort_value: v.sort_value && self.state.sel == UpId::SortValue.key(), ..v }
    }

    /// With no write of its own in flight, the rows read the stored value: the Details picker
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
// the preview
// ---------------------------------------------------------------------------------------------

/// The framed preview: a still row of three posters, the middle one lifted and captioned, in the
/// board's panel on the narrative column's floor. The row is [`SampleRow`], the shelves' own tile and
/// caption painters; the tiles are the first posters of the library the Library last showed
/// ([`plx_data::stores::browse::DirectoryView::sample`]), blank faces where it holds fewer.
const PREVIEW_TILES: usize = 3;
/// The tile the row lifts and captions when the library supplied enough posters to fill it.
const PREVIEW_LIFTED: usize = 1;
/// The Library's own card (250 x 375) with the board's 35 px between tiles: three of them are 820 px
/// wide, so centred in the 660 px frame the outer two stand 80 px past its edges and the frame clips them.
const PREVIEW_STYLE: RowStyle = RowStyle { gap: 35.0, ..RowStyle::HOME };
/// The board's frame: 660 x 556, resting on the narrative column's floor.
const PREVIEW_FRAME_W: f32 = 660.0;
const PREVIEW_FRAME_H: f32 = 556.0;
/// The air between the frame's top edge and the row.
const PREVIEW_PAD: f32 = 40.0;
/// How much of a tile that is not the lifted one shows (the board's 35%).
const PREVIEW_REST_ALPHA: f32 = 0.35;

/// The preview's frame: the whole block rests on the narrative column's floor.
fn preview_frame() -> Rect {
    let narrative = RouteLayout::screen().narrative;
    Rect::new(narrative.x, narrative.y + narrative.h - PREVIEW_FRAME_H, PREVIEW_FRAME_W, PREVIEW_FRAME_H)
}

/// Where the row of tiles stands: the frame less its top air, one tile tall.
fn preview_row() -> Rect {
    let frame = preview_frame();
    Rect::new(frame.x, frame.y + PREVIEW_PAD, frame.w, PREVIEW_STYLE.h)
}

/// The tile the row lifts: the middle one, or the last real poster when the library has fewer.
fn lifted_tile(posters: usize) -> usize {
    if posters == 0 { PREVIEW_LIFTED } else { PREVIEW_LIFTED.min(posters - 1) }
}

/// The poster the preview lifts and captions: the middle one, or the last real one when the library
/// has fewer ([`lifted_tile`]); `None` when it holds none.
fn lifted_poster<'a>(posters: &[&'a PmsMovie]) -> Option<&'a PmsMovie> {
    posters.get(lifted_tile(posters.len())).copied()
}

/// The facts the lifted tile is captioned from: its poster's own, or the sample when the library
/// has none to show.
fn caption_facts_of(poster: Option<&PmsMovie>) -> CaptionFacts {
    poster.map_or_else(sample_facts, tile_facts::caption_facts)
}

/// **The ONE source of every sample on the Under posters pages** (design review C1): the preview's
/// lifted poster's facts. The preview words them under the poster, and the picker's rows read their
/// samples off the same facts, so a row never promises "2019 · 2h 14m" over a preview that says
/// "2022 · 6m".
fn lifted_facts(posters: &[&PmsMovie]) -> CaptionFacts {
    caption_facts_of(lifted_poster(posters))
}

/// The year of the sample a preview with no poster behind it shows.
const SAMPLE_YEAR: i32 = 2019;

/// The sample facts a preview with no poster behind it is captioned from.
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

/// The preview's focused block for a stored value, built by the ONE builder every poster surface
/// uses ([`crate::registry::captions::label`]), so the preview cannot word, mark or hold a caption
/// differently from the screens it previews.
fn label_of(v: UnderPosters, facts: &CaptionFacts) -> TileLabel {
    crate::registry::captions::label(&caption_policy_of(v), facts, None, false)
}

/// The preview's caption lines for a stored value, as that block's words — what the lifted tile
/// wears. Pure, so a test states the exact words. `None` is the sample.
#[cfg(test)]
fn preview_lines(v: UnderPosters, poster: Option<&PmsMovie>) -> [Option<String>; 2] {
    lines_of(v, &caption_facts_of(poster))
}

/// [`preview_lines`] from facts already in hand.
fn lines_of(v: UnderPosters, facts: &CaptionFacts) -> [Option<String>; 2] {
    let label = label_of(v, facts);
    let text = |line: &Option<std::ffi::CString>| line.as_ref().map(|line| line.to_string_lossy().into_owned());
    [text(&label.title), text(&label.caption)]
}

/// The preview: a frame holding the row of posters, the lifted one wearing the caption the policy
/// gives it and the others dimmed. A face with no artwork is a blank face and carries no word, so the
/// title is only ever the caption. `foot` is the frame's own caption (the Sort value state's "Sorted
/// by Year", the picker's "The library you last opened"), `None` for none.
fn draw_preview(p: Painter, v: UnderPosters, posters: &[&PmsMovie], foot: Option<&str>,
    measure: &dyn plx_machine::machine::Measure) {
    let frame = SampleFrame { rect: preview_frame(), row: preview_row(), rest_alpha: PREVIEW_REST_ALPHA, foot };
    let lifted = lifted_tile(posters.len());
    let tiles = (0..PREVIEW_TILES).map(|i| Art::Poster(posters.get(i).map(|m| tile_facts::of(m))));
    if v.sort_value {
        // The Sort value row has focus: the row as a library sorted by Year draws it, every poster
        // wearing its own year (through the one year formatter), the lifted one bold. The preview
        // illustrates a Year-sorted library whatever kind the last-opened listing held (a TV show
        // library, its Seasons or Episodes, a Collections page), while the value table is per kind
        // (a season has no `year` term, so `label` would draw nothing under a season sample, and a
        // collection has no terms at all), so each sample is worded from its bare year through
        // `year_value`, not from the item.
        SampleRow::draw_values(p, &frame, &PREVIEW_STYLE, tiles, lifted,
            |i| crate::registry::sort_info::year_value(posters.get(i).map_or(SAMPLE_YEAR, |item| item.year)), measure);
    } else {
        let label = if band_h(&caption_policy_of(v)) > 0.0 {
            label_of(v, &caption_facts_of(lifted_poster(posters)))
        } else {
            TileLabel::default()
        };
        SampleRow::draw(p, &frame, &PREVIEW_STYLE, tiles, lifted, &label, measure);
    }
}

/// The posters a page's preview shows: the current library's first cached ones.
fn preview_posters<'a>(cx: &Cx<'a, InnerHost>) -> Vec<&'a PmsMovie> {
    cx.views.sample(PREVIEW_TILES).collect()
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
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
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

plx_ui::focusable_via_view!(UnderPostersPage, InnerHost, view);

impl Screen<InnerHost> for UnderPostersPage {
    fn name(&self) -> &'static str { "under-posters" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        let previewed = self.previewed();
        // Sort value On illustrates a library sorted by Year and says so; Off draws the Title/Details
        // caption and no footing.
        let foot = previewed.sort_value.then(plx_platform::i18n::msg::under_posters_sorted_by);
        draw_preview(f.painter, previewed, &preview_posters(f.cx), foot, f.cx.measure);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

// ---------------------------------------------------------------------------------------------
// the Details picker
// ---------------------------------------------------------------------------------------------

/// A details choice's row key: its place on the ladder, clear of the first page's rows.
#[derive(Clone, PartialEq, Eq, Debug)]
struct DetailsId(UnderPostersDetails);

impl FormId for DetailsId {
    fn key(&self) -> RowKey {
        RowKey(10 + self.0.index() as u32)
    }
}

/// What a choice shows under a poster, as the preview words it from `facts` ([`lifted_facts`]): the
/// row's sample on the right. `None` for Nothing; a kind the poster has no fact for samples the identity
/// line the tile says instead (its year), as the preview draws it.
fn details_sample(d: UnderPostersDetails, facts: &CaptionFacts) -> Option<String> {
    lines_of(UnderPosters { title: false, details: d, sort_value: false }, facts)[1].clone()
}

/// The picker's order: Nothing first, then the lines in the ladder's order (the board's list).
fn picker_order() -> impl Iterator<Item = UnderPostersDetails> {
    std::iter::once(UnderPostersDetails::None)
        .chain(UnderPostersDetails::LADDER.into_iter().filter(|d| *d != UnderPostersDetails::None))
}

/// The picker's rows: one checked choice per details line, the ladder's order, a sample of each
/// on the right, all from the preview's lifted poster.
fn details_form(current: UnderPostersDetails, facts: &CaptionFacts) -> Form<DetailsId, UnderPostersDetails, SettingsPage> {
    let mut section = FormSection::new("");
    for d in picker_order() {
        let mut row = Row::new(details_label(d));
        if let Some(sample) = details_sample(d, facts) {
            row = row.value(sample);
        }
        // The one choice a TV show cannot honour says what it does instead, as the board's copy rule
        // asks. The table has no focus-only detail (a detail line makes a row tall), so the row wears it always.
        if d == UnderPostersDetails::Video {
            row = row.detail(plx_platform::i18n::msg::under_posters_video_detail());
        }
        section = section.choice(DetailsId(d), d, row, |id| id.0 == current);
    }
    Form::new().section(section)
}

pub(super) struct DetailsPage {
    entry: EntryId,
    form: FormTable<DetailsId, UnderPostersDetails, SettingsPage>,
    state: DetailsState,
    /// What the rows' samples are worded from: the preview's lifted poster ([`lifted_facts`]),
    /// refreshed whenever the library's cached posters change.
    facts: CaptionFacts,
    /// A pick not yet answered, and the receipt of its write: the page pops when it is durable.
    pending: Option<(UnderPosters, std::sync::mpsc::Receiver<bool>)>,
}

struct DetailsState {
    value: UnderPosters,
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
    busy: bool,
}

impl DetailsState {
    /// The line the focused row stands for: what the preview shows.
    fn focused(&self) -> UnderPostersDetails {
        UnderPostersDetails::LADDER.into_iter().find(|d| DetailsId(*d).key() == self.sel).unwrap_or(self.value.details)
    }
}

impl LogicalState for DetailsState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).u8(self.value.pack()).bool(self.busy);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "under_posters_details sel={} details={} busy={}",
            self.sel.0, self.value.details.index(), self.busy
        ));
    }
}

impl DetailsPage {
    pub(super) fn new(entry: EntryId) -> Self {
        let value = plx_media::route::under_posters();
        let mut page = Self {
            entry,
            form: FormTable::new(super::super::registry::BAND),
            state: DetailsState { value, sel: DetailsId(value.details).key(), busy: false },
            facts: sample_facts(),
            pending: None,
        };
        page.rebuild();
        page
    }

    /// Word the rows' samples from the preview's lifted poster as it is now; `true` when they changed.
    fn adopt(&mut self, cx: &Cx<'_, InnerHost>) -> bool {
        let facts = lifted_facts(&preview_posters(cx));
        if facts == self.facts {
            return false;
        }
        self.facts = facts;
        self.rebuild();
        true
    }

    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(DetailsId(self.state.value.details));
        self.form.table.compact = false;
        self.form.set(details_form(self.state.value.details, &self.facts), Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        use plx_platform::i18n::msg;
        TableScreen::new(
            Header::new(RouteLayout::screen(), Some(msg::under_posters_title()), msg::under_posters_details_row(), msg::under_posters_details_copy()),
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
        if details == self.state.value.details {
            Self::pop(fx);
            return;
        }
        let next = UnderPosters { details, ..self.state.value };
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
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
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
        plx_ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
        // The picker chooses the Details line, so its preview words that line: the sort caption would
        // hide every choice behind the same words.
        let previewed = UnderPosters { details: self.state.focused(), sort_value: false, ..self.state.value };
        draw_preview(f.painter, previewed, &preview_posters(f.cx), Some(plx_platform::i18n::msg::under_posters_last_opened()), f.cx.measure);
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
