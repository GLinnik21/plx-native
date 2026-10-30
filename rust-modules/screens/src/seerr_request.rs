//! **The request menu** (docs/seerr.md): where a Seerr request goes — the Radarr / Sonarr server,
//! its quality profile and its root folder — opened by the Seerr title page's Request pill and the
//! Detail page's request disc for a user Seerr lets choose them
//! (`SeerrStore::may_choose_options`; anyone else's press sends at once, as before).
//!
//! A `Style::Compact` surface anchored to the control that opened it, the Play Version panel's shape
//! (`screens::versions_panel`). It asks the Seerr store for the servers of the title's kind when it
//! mounts, and for a server's profiles and folders once that server is the one chosen, and rebuilds
//! its rows as each answer lands. Every choice starts on what the server would pick unasked (its
//! default server, that server's default profile and folder, the anime pair for an anime show):
//! [`effective`]. The first row sends the request and names only what the viewer changed
//! ([`request_options`]), so a choice left alone stays the server's, and its override rules'.
//!
//! The request is asked on the title's own `Slot::Request`, the slot the page's own press uses, so
//! the page under the menu follows its answer (Requesting…, Requested, why it failed) unchanged.

use std::borrow::Cow;
use std::convert::Infallible;

use plx_data::seerr::{default_server, ArrOptions, ArrServer, MediaKind, RequestOptions, SeerrData, SeerrOp};
use plx_data::stores::seerr::{offer, Answer, Offer, SeerrCmd, SeerrStore, Slot};
use plx_data::stores::{StoreCmd, StoreId};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key, LogicalState, Machine, NavOp,
};
use plx_platform::i18n::msg;
use plx_ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey};
use plx_ui::frame::Budget;
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step, Stop,
};
use plx_ui::table::Row;
use plx_ui::Rect;

use crate::registry::{AppFx, SearchLike};

/// The fields [`SeerrRequestScreen::write`] canonicalises, for the recorder's shape pin (§5.4).
pub const SHAPE: &str = "SeerrRequest{arg:SeerrRequestArg{tv:bool,tmdb:u64,anchor:[u32;4]},\
    choices:{server:opt<i64>,profile:opt<i64>,folder:opt<str>},asked:opt<i64>,sel:i32,table:TableViewMotion}";

/// What the container is asked to present: the title the request is for, and the drawn rect of the
/// control the menu hangs off (bit-preserving, so a canonical argument holds it without float
/// equality).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SeerrRequestArg {
    pub kind: MediaKind,
    pub tmdb: i64,
    pub anchor: [u32; 4],
}

impl LogicalState for SeerrRequestArg {
    fn write(&self, c: &mut Canon) {
        c.bool(self.kind == MediaKind::Tv).u64(self.tmdb as u64);
        for value in self.anchor {
            c.u32(value);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str("seerr_request_arg");
    }
}

/// A row's identity. The lists are the server's, so their rows are keyed by position in the answer
/// they were built from (a dynamic list's documented key), each list in its own band.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RowId {
    Send,
    Server(u16),
    Profile(u16),
    Folder(u16),
}

impl FormId for RowId {
    fn key(&self) -> RowKey {
        RowKey(match *self {
            RowId::Send => 0,
            RowId::Server(i) => 0x1_0000 + u32::from(i),
            RowId::Profile(i) => 0x2_0000 + u32::from(i),
            RowId::Folder(i) => 0x3_0000 + u32::from(i),
        })
    }
}

/// What a row does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Act {
    Send,
    Server(i64),
    Profile(i64),
    Folder(String),
}

/// What the viewer picked; `None` is "what the server would choose".
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Choices {
    pub server: Option<i64>,
    pub profile: Option<i64>,
    pub folder: Option<String>,
}

/// The servers this client's requests can go to: the non-4K ones (it never sends a 4K request).
fn offered(servers: &[ArrServer]) -> impl Iterator<Item = &ArrServer> {
    servers.iter().filter(|s| !s.is4k)
}

/// The choice in force for each row: the viewer's, else the server's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective<'a> {
    pub server: Option<&'a ArrServer>,
    pub profile: Option<i64>,
    pub folder: Option<String>,
}

/// What each row stands on: the chosen server (one of [`offered`]) or the default one, and on that
/// server the chosen profile and folder or its defaults for this title ([`ArrServer::defaults`]).
pub fn effective<'a>(servers: &'a [ArrServer], anime: bool, choices: &Choices) -> Effective<'a> {
    let server = choices.server.and_then(|id| offered(servers).find(|s| s.id == id)).or_else(|| default_server(servers));
    let (profile, folder) = server.map_or((None, None), |s| s.defaults(anime));
    Effective {
        server,
        profile: choices.profile.or(profile),
        folder: choices.folder.clone().or_else(|| folder.map(str::to_owned)),
    }
}

/// What the request names: only the choices that differ from what the server would pick, so a row
/// the viewer left alone stays the server's to decide (and its override rules').
pub fn request_options(servers: &[ArrServer], anime: bool, choices: &Choices) -> RequestOptions {
    let chosen = effective(servers, anime, choices);
    let default_id = default_server(servers).map(|s| s.id);
    let (profile, folder) = chosen.server.map_or((None, None), |s| s.defaults(anime));
    RequestOptions {
        server: chosen.server.map(|s| s.id).filter(|id| Some(*id) != default_id),
        profile: choices.profile.filter(|p| Some(*p) != profile),
        folder: choices.folder.clone().filter(|f| Some(f.as_str()) != folder),
    }
}

/// One of the store's answers as the menu reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load<T> {
    Loading,
    Failed,
    Ready(T),
}

impl<'a, T> Load<&'a T> {
    fn of(answer: Option<&'a Answer>, read: impl Fn(&'a SeerrData) -> Option<&'a T>) -> Self {
        match answer {
            Some(Answer::Ready(data)) => read(data).map_or(Load::Failed, Load::Ready),
            Some(Answer::Failed(_)) => Load::Failed,
            Some(Answer::Loading) | None => Load::Loading,
        }
    }
}

/// The menu's rows: Send first (the menu opens on it, so OK, OK sends what the server would pick),
/// then the servers when there is more than one to choose from, then the chosen server's quality
/// profiles and root folders, each checked on the choice in force. A list still on its way, or one
/// that could not be read, is a note; Send stays.
pub fn form(
    send_label: &str,
    servers: Load<&Vec<ArrServer>>,
    options: Load<&ArrOptions>,
    chosen: &Effective<'_>,
) -> Form<RowId, Act, Infallible> {
    // placeholder-exempt: SCOPE decision, not a non-placeholder: this is a real wait caption, left uncounted because no demo storyboard or screenshot scene opens the request menu (a driver that does must count it)
    let waiting = msg::seerr_menu_loading();
    let mut form = Form::new().section(FormSection::new("").item(RowId::Send, plx_ui::form::RowKind::Button, Act::Send,
        Row::new(send_label)));
    let servers = match servers {
        Load::Ready(servers) => servers,
        Load::Loading => return form.section(FormSection::new(msg::seerr_menu_quality()).note(waiting)),
        Load::Failed => return form.section(FormSection::new(msg::seerr_menu_quality()).note(msg::seerr_menu_unavailable())),
    };
    let choosable: Vec<&ArrServer> = offered(servers).collect();
    if choosable.is_empty() {
        return form;
    }
    if choosable.len() > 1 {
        let mut section = FormSection::new(msg::seerr_menu_server());
        for (i, server) in choosable.iter().enumerate() {
            section = section.choice(RowId::Server(i as u16), Act::Server(server.id), Row::new(server.name.as_str()).server_label(),
                |_| chosen.server.is_some_and(|s| s.id == server.id));
        }
        form = form.section(section);
    }
    let options = match options {
        Load::Ready(options) => options,
        Load::Loading => return form.section(FormSection::new(msg::seerr_menu_quality()).note(waiting)),
        Load::Failed => return form.section(FormSection::new(msg::seerr_menu_quality()).note(msg::seerr_menu_unavailable())),
    };
    let mut quality = FormSection::new(msg::seerr_menu_quality()).visible(!options.profiles.is_empty());
    for (i, profile) in options.profiles.iter().enumerate() {
        quality = quality.choice(RowId::Profile(i as u16), Act::Profile(profile.id), Row::new(profile.name.as_str()).server_label(),
            |_| chosen.profile == Some(profile.id));
    }
    let mut folders = FormSection::new(msg::seerr_menu_folder()).visible(!options.folders.is_empty());
    for (i, folder) in options.folders.iter().enumerate() {
        let mut row = Row::new(folder.path.as_str()).server_label();
        if let Some(free) = folder.free.filter(|b| *b > 0) {
            row = row.detail(msg::seerr_menu_free(&plx_ui::fmt::bytes(free, (0, 0, 0))));
        }
        folders = folders.choice(RowId::Folder(i as u16), Act::Folder(folder.path.clone()), row,
            |_| chosen.folder.as_deref() == Some(folder.path.as_str()));
    }
    form.section(quality).section(folders)
}

/// The pinned ~20px corner radius, the item menu's and Play Version's.
const PANEL_RAD: f32 = 20.0;
const RISE: f32 = plx_ui::popover::Popover::RISE;

pub struct SeerrRequestScreen {
    entry: EntryId,
    arg: SeerrRequestArg,
    choices: Choices,
    /// The server whose profiles and folders were asked for: asked once per server the menu stands on.
    asked: Option<i64>,
    /// The store generation and choices the rows were last built from.
    built: Option<(u32, Choices)>,
    pub form: FormTable<RowId, Act, Infallible>,
}

impl SeerrRequestScreen {
    pub fn new(entry: EntryId, arg: SeerrRequestArg) -> Self {
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        // folder rows carry a sub-line: the track menu's size class
        form.table.compact = false;
        Self { entry, arg, choices: Choices::default(), asked: None, built: None, form }
    }

    fn ask<H: SearchLike>(&self, slot: Slot, op: SeerrOp, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))));
    }

    fn anime(&self, store: &SeerrStore) -> bool {
        store.title_detail(self.arg.kind, self.arg.tmdb).is_some_and(|d| d.anime)
    }

    fn servers<'a>(&self, store: &'a SeerrStore) -> &'a [ArrServer] {
        store.servers(self.arg.kind).unwrap_or(&[])
    }


    /// Ask for the chosen server's profiles and folders once it is known, and rebuild the rows when
    /// an answer or a choice changed what they say.
    fn follow<H: SearchLike>(&mut self, store: &SeerrStore, fx: &mut Effects<'_, H>) {
        let (kind, anime) = (self.arg.kind, self.anime(store));
        let chosen = effective(self.servers(store), anime, &self.choices).server.map(|s| s.id);
        if let Some(server) = chosen.filter(|id| self.asked != Some(*id)) {
            self.asked = Some(server);
            self.ask(Slot::ServerOptions(kind, server), SeerrOp::ServerOptions { kind, server }, fx);
        }
        let stamp = (store.gen(), self.choices.clone());
        if self.built.as_ref() == Some(&stamp) {
            return;
        }
        let first = self.built.is_none();
        self.built = Some(stamp);
        let servers = Load::of(store.answer(&Slot::Servers(kind)), |d| match d {
            SeerrData::Servers(s) => Some(s),
            _ => None,
        });
        let options = match chosen {
            Some(server) => Load::of(store.answer(&Slot::ServerOptions(kind, server)), |d| match d {
                SeerrData::ServerOptions(o) => Some(o),
                _ => None,
            }),
            None => Load::Loading,
        };
        // Send says only "Request": the control that opened the menu already said what is asked for
        // ("Request missing seasons"), and the long form does not fit a menu in every language.
        let form = form(msg::seerr_title_request(), servers, options, &effective(self.servers(store), anime, &self.choices));
        if first {
            self.form.open(form, Some(&RowId::Send));
        } else {
            self.form.refresh(form);
        }
        fx.invalidate(plx_machine::present::Provenance::Nav);
    }

    /// Under the control when there is room, above it when there is not.
    pub fn frame(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        plx_ui::popover::under_or_above(Rect::new(x, y, w, h), self.form.table.measured_width(measure), self.form.table.measured_height())
    }

    fn dismiss<H: SearchLike>(&self, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// Row `elem`: a choice moves the check (a new server starts its profile and folder over on
    /// its own defaults), Send asks for what the title still offers and closes the menu. Takes the
    /// elem, not the cursor: a click names the stop it hit.
    fn commit<H: SearchLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let act = match self.form.index_of_key(RowKey(elem)).and_then(|i| self.form.activate(i)) {
            Some(Activation::Action(act)) => act,
            _ => return,
        };
        match act {
            Act::Server(id) => {
                if self.choices.server != Some(id) {
                    self.choices = Choices { server: Some(id), ..Choices::default() };
                }
            }
            Act::Profile(id) => self.choices.profile = Some(id),
            Act::Folder(path) => self.choices.folder = Some(path),
            Act::Send => {
                self.dismiss(fx);
                let Some(store) = H::seerr(cx) else { return };
                let (kind, tmdb) = (self.arg.kind, self.arg.tmdb);
                let Some(detail) = store.title_detail(kind, tmdb) else { return };
                if let Offer::Request(seasons) = offer(detail, store.answer(&Slot::Request(kind, tmdb))) {
                    let options = request_options(self.servers(store), detail.anime, &self.choices);
                    self.ask(Slot::Request(kind, tmdb), SeerrOp::Request { kind, tmdb, seasons, options }, fx);
                }
                return;
            }
        }
        if let Some(store) = H::seerr(cx) {
            self.follow(store, fx);
        }
    }
}

impl<H: SearchLike> Machine<H> for SeerrRequestScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                let kind = self.arg.kind;
                self.ask(Slot::Servers(kind), SeerrOp::Servers { kind }, fx);
                Handled::Yes
            }
            ScreenEvent::Tick(tick) => {
                if let Some(store) = H::seerr(cx) {
                    self.follow(store, fx);
                }
                self.form.table.sel = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .and_then(|key| self.form.index_of_key(RowKey(key.elem)))
                    .map_or(self.form.table.sel, |i| i as i32);
                self.form.table.update(tick.dt(), self.frame(cx.measure).h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                if let Some(i) = self.form.index_of_key(RowKey(to.elem)) {
                    self.form.table.sel = i as i32;
                }
                self.form.note_engine_key(Some(RowKey(to.elem)));
                fx.invalidate(plx_machine::present::Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.commit(*elem, cx, fx);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current.filter(|k| k.entry == self.entry) {
                    self.commit(key.elem, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(input) => match input.kind {
                InputKind::Key { key: Key::Back, edge: Edge::Down, .. } => {
                    self.dismiss(fx);
                    Handled::Yes
                }
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

/// One column of `Bare` rows over the table's own cursor, Play Version's `Focusable`.
impl<H: SearchLike> Focusable<H> for SeerrRequestScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(cx.measure),
            len: self.form.focusable_len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.form.index_of_key(RowKey(*elem)).map(|_| GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let delta = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return Step::Edge,
        };
        match self.form.step_key(RowKey(key.elem), delta) {
            Some(next) => Step::Move(FocusKey { entry: self.entry, elem: next.0 }),
            None => Step::Edge,
        }
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let index = self.form.index_of_key(RowKey(*elem))?;
        let rect = self.form.table.row_frame(self.frame(cx.measure), index as i32)?;
        Some(Placed { rect, rest_rect: rect, clip: self.frame(cx.measure), index: Some(index as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            FocusKey { entry: self.entry, elem: self.form.selected_key().or_else(|| self.form.opening_key()).map_or(0, |k| k.0) }
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self.form.selected_key().or_else(|| self.form.opening_key()).map_or(0, |k| k.0),
        }
    }
}

impl LogicalState for SeerrRequestScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        for value in [self.choices.server, self.choices.profile] {
            match value {
                Some(v) => c.bool(true).u64(v as u64),
                None => c.bool(false).u64(0),
            };
        }
        match &self.choices.folder {
            Some(path) => c.bool(true).str(path),
            None => c.bool(false).str(""),
        };
        match self.asked {
            Some(v) => c.bool(true).u64(v as u64),
            None => c.bool(false).u64(0),
        };
        c.u32(self.form.table.sel as u32);
        self.form.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("seerr_request");
    }
}

impl<H: SearchLike> Screen<H> for SeerrRequestScreen {
    fn name(&self) -> &'static str {
        "seerr_request"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn scrim(&self) -> Scrim {
        Scrim::dim(plx_ui::theme::underlay::DIM_PANEL)
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let appear = f.page_alpha;
        let r = self.frame(f.measure);
        let p = f.painter.alpha(appear).translate(0.0, RISE * (1.0 - appear));
        let measure = f.measure;
        let field = f.underlay;
        plx_ui::profile::phase("dt.seerr_request", || {
            plx_ui::widgets::panel_ground(p, r, PANEL_RAD, field, plx_ui::widgets::Ground::Sheet);
            self.form.table.draw(p, r, measure);
        });
        let hit_p = f.painter.alpha(appear);
        for index in 0..self.form.table.n_rows() as usize {
            let Some(elem) = self.form.key_at(index).map(|k| k.0) else {
                continue;
            };
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                f.stop(
                    hit_p,
                    Stop {
                        key: FocusKey { entry: self.entry, elem },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "seerr_request_tests.rs"]
mod tests;
