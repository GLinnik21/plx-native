//! **Settings > Libraries > Under posters**: what the focused poster shows beneath it.
//!
//! One global setting, saved per profile (`plx_plex::plex::session::UnderPosters`): a Title
//! switch, one choice among the details lines (Year · Length by default), Reset, and a live
//! preview of sample tiles drawn by the SAME policy the shelves and the Library read
//! ([`plx_ui::caption_policy`]). The preview is the page's proof: it is built from the value the
//! rows show, so a pick moves it in the frame the row changes.
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
    Details(UnderPostersDetails),
    Reset,
}

impl FormId for UpId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            UpId::Title => 1,
            UpId::Details(d) => 10 + d.index() as u32,
            UpId::Reset => 30,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpAction {
    Title,
    Pick(UnderPostersDetails),
    Reset,
}

/// The page's rows from plain inputs (the text-fit tests call this directly).
fn under_posters_form(v: UnderPosters) -> Form<UpId, UpAction, SettingsPage> {
    use plx_platform::i18n::msg;
    let title = FormSection::new("").item(
        UpId::Title,
        RowKind::Toggle,
        UpAction::Title,
        Row::new(msg::under_posters_title_row()).detail(msg::under_posters_title_detail()).toggle(v.title),
    );
    let mut details = FormSection::new(msg::under_posters_details_section());
    for d in UnderPostersDetails::LADDER {
        details = details.choice(UpId::Details(d), UpAction::Pick(d), Row::new(details_label(d)),
            |id| *id == UpId::Details(v.details));
    }
    let reset = FormSection::new("").item(
        UpId::Reset,
        RowKind::Button,
        UpAction::Reset,
        Row::new(msg::under_posters_reset()).dim(v == UnderPosters::DEFAULT),
    );
    Form::new().section(title).section(details).section(reset)
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
            UpAction::Pick(details) => UnderPosters { details, ..v },
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

/// The sample poster's width in the preview: the Library's tile at 56%, so the proportions
/// (width : height : gap) are the Library's own.
const PREVIEW_W: f32 = 140.0;
const PREVIEW_SCALE: f32 = PREVIEW_W / plx_ui::consts::CARD_W;
const PREVIEW_H: f32 = plx_ui::consts::CARD_H * PREVIEW_SCALE;
const PREVIEW_GAP: f32 = plx_ui::consts::GAP * PREVIEW_SCALE;
const PREVIEW_TILES: usize = 3;
/// The room under the tiles: the label's drop plus the tallest band.
const PREVIEW_UNDER: f32 = 30.0 + plx_ui::caption_policy::BAND_TWO_LINES;

/// Where the preview's tiles start: the whole block rests on the narrative column's floor.
fn preview_top() -> f32 {
    let narrative = RouteLayout::screen().narrative;
    narrative.y + narrative.h - PREVIEW_UNDER - PREVIEW_H - theme::space::SM
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

/// The preview: a row of sample posters, the middle one focused, wearing the caption the policy
/// gives it. A poster with no artwork shows its title inside the placeholder, so the title is
/// never lost when the caption leaves it out.
fn draw_preview(p: Painter, v: UnderPosters, measure: &dyn plx_machine::machine::Measure) {
    let narrative = RouteLayout::screen().narrative;
    let top = preview_top();
    let sty = RowStyle { w: PREVIEW_W, h: PREVIEW_H, gap: PREVIEW_GAP, ..RowStyle::HOME };
    let name = std::ffi::CString::new(plx_platform::i18n::msg::under_posters_sample_title()).unwrap_or_default();
    let size = theme::size::CAPTION;
    let inner = PREVIEW_W - 2.0 * theme::space::SM;
    for i in 0..PREVIEW_TILES {
        let x = narrative.x + i as f32 * (PREVIEW_W + PREVIEW_GAP);
        let r = Rect::new(x, top, PREVIEW_W, PREVIEW_H);
        p.rrect_sheened(r, theme::CARD_RING_RAD * PREVIEW_SCALE, theme::CARD_ABSENT);
        let fit = measure.fit_line(&name.to_string_lossy(), inner, size, true);
        Label::new(fit.as_ptr(), size, theme::TEXT_SECONDARY).bold().h(HAlign::Center).v(VAlign::Middle)
            .draw(p, Rect::new(x + theme::space::SM, top, inner, PREVIEW_H));
    }
    let focused = Rect::new(narrative.x + (PREVIEW_W + PREVIEW_GAP), top, PREVIEW_W, PREVIEW_H);
    if band_h(&caption_policy_of(v)) > 0.0 {
        draw_label(p, focused, 1.0, &sty, &TileLabel::from_lines(preview_lines(v)), measure);
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

#[cfg(test)]
#[path = "settings_under_posters_tests.rs"]
mod tests;
