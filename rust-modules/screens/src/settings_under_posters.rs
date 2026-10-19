//! **Settings > Libraries > Under posters**: what the focused poster shows beneath it.
//!
//! One global setting, saved per profile (`plx_plex::plex::session::UnderPosters`). The page holds
//! three rows: a Title switch, a Details row that opens [`DetailsPage`] (its pushed picker, one
//! checked choice per details line, each with a sample of what it shows) and Reset. Both pages carry
//! a framed preview of one lifted sample poster and its caption, drawn by the SAME policy the
//! shelves and the Library read ([`plx_ui::caption_policy`]). The preview is built from the value
//! the rows show, so a change moves it in the frame the row changes; the picker's preview follows
//! the focused choice.
//!
//! The write goes through the one door every live setting uses (`PreferenceCmd::Choose`, Confirm):
//! the row shows the pick at once, the stored value settles it when the receipt answers, and a
//! refused write (nobody seated) puts the row back.

use super::*;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_ui::caption_policy::{band_h, caption_lines, CaptionFacts, CaptionPolicy, DetailsKind};
use plx_ui::cards::{draw_label, RowStyle, TileLabel};
use plx_ui::label::{HAlign, Label, VAlign};

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

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpId {
    Title,
    Details,
    Reset,
}

impl FormId for UpId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            UpId::Title => 1,
            UpId::Details => 2,
            UpId::Reset => 30,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpAction {
    Title,
    /// The Details row is a push (`RowKind::Nav`): the form answers it with the page, never with this.
    Details,
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
            "under_posters sel={} title={} details={} busy={}",
            self.sel.0, self.value.title, self.value.details.index(), self.busy
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
            UpAction::Reset => UnderPosters::DEFAULT,
        };
        self.send(next, fx);
    }

    /// Settle an answered write: the row reads the stored value again, which holds the pick only
    /// if the write was durable.
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

// ---------------------------------------------------------------------------------------------
// the preview
// ---------------------------------------------------------------------------------------------

/// The framed preview: one lifted sample poster and its caption, the board's panel on the narrative
/// column's floor. The poster is the Library's tile proportions at a size that leaves the tallest
/// caption band (title and details, 112) clear under it inside the frame.
const PREVIEW_W: f32 = 240.0;
const PREVIEW_H: f32 = PREVIEW_W * plx_ui::consts::CARD_H / plx_ui::consts::CARD_W;
/// The air between the frame's top edge and the poster.
const PREVIEW_PAD: f32 = 32.0;
/// The room under the poster: the label's drop plus the tallest band.
const PREVIEW_UNDER: f32 = 30.0 + plx_ui::caption_policy::BAND_TWO_LINES;
/// The frame's height: the poster and its caption band with air above and below.
const PREVIEW_FRAME_H: f32 = PREVIEW_PAD + PREVIEW_H + PREVIEW_UNDER + PREVIEW_PAD;

/// The preview's frame: the whole block rests on the narrative column's floor.
fn preview_frame() -> Rect {
    let narrative = RouteLayout::screen().narrative;
    Rect::new(narrative.x, narrative.y + narrative.h - PREVIEW_FRAME_H, narrative.w, PREVIEW_FRAME_H)
}

/// The lifted poster, centred in the frame.
fn preview_poster() -> Rect {
    let frame = preview_frame();
    Rect::new(frame.x + (frame.w - PREVIEW_W) * 0.5, frame.y + PREVIEW_PAD, PREVIEW_W, PREVIEW_H)
}

/// The sample facts every preview tile is captioned from.
fn sample_facts() -> CaptionFacts {
    use plx_platform::i18n::msg;
    CaptionFacts {
        title: Some(msg::under_posters_sample_title().to_string()),
        year: Some(2019),
        duration_min: Some(134),
        genre: Some(msg::under_posters_sample_genre().to_string()),
        video_resolution: Some("4k".to_string()),
        hdr: true,
        content_rating: Some("PG-13".to_string()),
        audience_rating: Some(7.8),
        ..Default::default()
    }
}

/// The preview's caption lines for a stored value — what the focused sample tile wears. Pure, so
/// a test states the exact words.
fn preview_lines(v: UnderPosters) -> [Option<String>; 2] {
    caption_lines(&caption_policy_of(v), &sample_facts(), plx_platform::i18n::current())
}

/// The preview: a frame holding one lifted sample poster, wearing the caption the policy gives
/// it. A poster with no artwork shows its title inside the placeholder, so the title is never lost
/// when the caption leaves it out.
fn draw_preview(p: Painter, v: UnderPosters, measure: &dyn plx_machine::machine::Measure) {
    let frame = preview_frame();
    p.rect(frame, theme::CARD_RING_RAD, theme::scrim_black(0.30), theme::scrim_black(0.30), 0.0);
    let r = preview_poster();
    let sty = RowStyle { w: PREVIEW_W, h: PREVIEW_H, ..RowStyle::HOME };
    // placeholder-exempt: a fixed illustration of the caption policy, not a content stand-in for a pending load
    p.rrect_sheened(r, theme::CARD_RING_RAD, theme::CARD_ABSENT);
    let name = plx_platform::i18n::msg::under_posters_sample_title();
    let size = theme::size::LABEL;
    let inner = PREVIEW_W - 2.0 * theme::space::SM;
    let fit = measure.fit_line(name, inner, size, true);
    Label::new(fit.as_ptr(), size, theme::TEXT_SECONDARY).bold().h(HAlign::Center).v(VAlign::Middle)
        .draw(p, Rect::new(r.x + theme::space::SM, r.y, inner, r.h));
    if band_h(&caption_policy_of(v)) > 0.0 {
        draw_label(p, r, 1.0, &sty, &TileLabel::from_lines(preview_lines(v)), measure);
    }
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
        draw_preview(f.painter, self.state.value, f.cx.measure);
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

/// What a choice shows under a poster, as the preview words it: the row's sample on the right.
/// `None` for Nothing, whose row has no value.
fn details_sample(d: UnderPostersDetails) -> Option<String> {
    preview_lines(UnderPosters { title: false, details: d })[1].clone()
}

/// The picker's order: Nothing first, then the lines in the ladder's order (the board's list).
fn picker_order() -> impl Iterator<Item = UnderPostersDetails> {
    std::iter::once(UnderPostersDetails::None)
        .chain(UnderPostersDetails::LADDER.into_iter().filter(|d| *d != UnderPostersDetails::None))
}

/// The picker's rows: one checked choice per details line, the ladder's order, a sample of each
/// on the right.
fn details_form(current: UnderPostersDetails) -> Form<DetailsId, UnderPostersDetails, SettingsPage> {
    let mut section = FormSection::new("");
    for d in picker_order() {
        let mut row = Row::new(details_label(d));
        if let Some(sample) = details_sample(d) {
            row = row.value(sample);
        }
        section = section.choice(DetailsId(d), d, row, |id| id.0 == current);
    }
    Form::new().section(section)
}

pub(super) struct DetailsPage {
    entry: EntryId,
    form: FormTable<DetailsId, UnderPostersDetails, SettingsPage>,
    state: DetailsState,
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
            pending: None,
        };
        page.rebuild();
        page
    }

    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(DetailsId(self.state.value.details));
        self.form.table.compact = false;
        self.form.set(details_form(self.state.value.details), Some(&keep));
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
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if self.poll(fx) {
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
        draw_preview(f.painter, UnderPosters { details: self.state.focused(), ..self.state.value }, f.cx.measure);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

#[cfg(test)]
#[path = "settings_under_posters_tests.rs"]
mod tests;
