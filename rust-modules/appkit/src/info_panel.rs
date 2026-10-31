//! In-player Info card (mockup "Info mode"): a horizontal card over the transport with the
//! episode/movie still, title + synopsis, a metadata line with outlined capability badges, and a
//! column of action buttons. Opened from the HUD's "Info" tab; app.rs routes D-pad/OK/BACK here
//! while it's open and hides the normal transport middle behind it. Data from plx_data::metadata.
#![allow(dead_code)]
use crate::chip_row::{draw_chip_row, Chip, ChipRow};
use plx_data::metadata;
use plx_ui::consts::{SCR_H, SCR_W};
use plx_ui::frame::Budget;
use plx_ui::geom::IndexElem;
use plx_ui::hstack::Item;
use plx_ui::icons::Icon;
use plx_machine::machine::{Cx, EntryId, FocusKey, GroupId, Host};
use plx_ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Part, Placed, Seat, Step, Stop,
};
use plx_ui::text_view::TextView;
use plx_ui::theme;
use plx_ui::vstack::{Anchor, VStack};
use plx_ui::widgets::resolve_tex_wh_on;
use plx_ui::{Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_int;

/// [`actions`]'s fixed length: it always returns two labels, only their TEXT depends on
/// `is_episode`. Every caller that only needs the count (focus bounds, group sizing) uses this
/// constant instead of building the whole array, which is what lets those call sites avoid
/// needing a `MetadataView` at all.
const N_ACTIONS: usize = 2;

#[derive(Clone, Debug, PartialEq)]
pub enum InfoAction {
    None,
    FromBeginning,
    GoToDetail(String), // rk to open: the show (episode) or the movie
}

/// The card's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw`
/// takes the appear fraction as a parameter instead of stepping its own `Popover`.
pub struct InfoPanelState {
    focus: c_int, // index into the action-button column
    /// The action column's FOCUS POP — one spring per button ([`plx_ui::widgets::CtlPop`]). Two,
    /// the whole of [`actions`].
    ctl_pop: plx_ui::widgets::CtlPop<2>,
}

/// The ratingKey of the page the playing item belongs to: the show for an episode, the movie
/// itself otherwise. What the Info card's second action opens, and what the `go_to_detail`
/// command opens and is enabled by. `None` when nothing playing has one.
pub fn detail_rk<'a>(meta: metadata::MetadataView<'a>) -> Option<&'a str> {
    meta.now_playing()
        .map(|n| n.detail_rk.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| meta.current().map(|d| d.rk.as_str()))
        .filter(|s| !s.is_empty())
}

impl InfoPanelState {
    pub fn new() -> Self {
        InfoPanelState {
            focus: 0,
            ctl_pop: plx_ui::widgets::CtlPop::new(),
        }
    }

    /// The focused action button, for the focus probe (`crate::focusprobe`). Same reason as the
    /// other panels: the card's UP/DOWN arm moves this alone, so nothing else in the log would show
    /// it.
    pub fn sel(&self) -> c_int {
        self.focus
    }

    /// true when focus is on the last action button — a further DOWN should leave the card (back to
    /// the tabs) rather than staying pinned to the bottom row
    pub fn at_last(&self) -> bool {
        self.focus >= N_ACTIONS as c_int - 1
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`InfoPanelPart`] answers is the source of geometry, but the ENGINE owns the current
    /// element (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    pub fn set_focus(&mut self, i: c_int) {
        self.focus = i;
    }

    /// activate the focused action — dismissing the card afterward is the container's job now, not
    /// this method's.
    pub fn on_ok(&self, meta: metadata::MetadataView<'_>) -> InfoAction {
        let f = self.focus;
        if f <= 0 {
            return InfoAction::FromBeginning;
        }
        // second action opens the show (episode) or the movie
        detail_rk(meta).map_or(InfoAction::None, |rk| InfoAction::GoToDetail(rk.to_owned()))
    }

    /// **Which action button `focus` is on, if it is in the column at all** — asked by
    /// [`update`](Self::update) to decide which button `ctl_pop` pops. Under [`InfoPanelPart`]'s
    /// Engine groups this is always `Some`: the card's only focus group IS the action column
    /// (`ElemKind::Control`), so a focus that reaches this screen at all is always on a control
    /// face — there is no longer a "tabs above" state sharing `focus`'s range to distinguish it
    /// from (restructure phase 12 retired the `focus_is_ctl` predicate this used to also answer,
    /// once `screens::player::overlay::PlayerOverlayScreen`'s own `Key::Ok` arm stopped asking it:
    /// every `ScreenEvent::PressCommit` this card's engine-armed press delivers already IS a
    /// control-face press).
    fn ctl_index(&self) -> Option<usize> {
        usize::try_from(self.focus)
            .ok()
            .filter(|&i| i < N_ACTIONS)
    }

    pub fn update(&mut self, dt: f32) {
        // The card's focus walks the tabs above these buttons too, and `focus` is the button index
        // only while it is inside the column — outside it, every pop closes.
        let idx = self.ctl_index();
        self.ctl_pop.step(idx, dt);
    }

    pub fn draw(
        &mut self,
        ps: &plx_media::route::PlaybackSession,
        appear: f32,
        measure: &dyn plx_machine::machine::Measure,
        meta: metadata::MetadataView<'_>,
    ) {
        let Some(c) = info_content(meta) else { return };
        // no scrim — the card floats over the transport; reproduces exactly what
        // `Popover::painter(0.0, 20.0)` (i.e. no scrim + `Popover::content_painter(20.0)`) drew.
        let p = Painter::root()
            .alpha(appear)
            .translate(0.0, plx_ui::popover::Popover::RISE * (1.0 - appear));
        let thumb_path = c.thumb.clone();

        // card — tall enough that the still gets equal padding on every side (see `pad`/`sh` below)
        let (card, pad) = card_geometry();
        let (cx, cyt, _cw, ch) = (card.x, card.y, card.w, card.h);
        // near-opaque dark card keeps the title/synopsis legible over any scene
        let cardbg = theme::PANEL_TOP;
        p.rrect(card, 28.0, 28.0, cardbg);

        // still (16:9), left — the *episode's* thumbnail (or the movie's landscape art). `ch` is sized
        // so (ch - sh)/2 == pad, giving the still an equal `pad` margin on every side.
        let sw = 320.0f32;
        let sh = 180.0f32;
        let sx = cx + pad;
        let sy = cyt + (ch - sh) * 0.5;
        let mut drawn = false;
        if !thumb_path.is_empty() {
            // the PLAYING item's server — the info panel describes what is on the video plane
            let (t, tw, th) = resolve_tex_wh_on(
                plx_media::route::item_sid(plx_media::route::cur_sid(ps)).raw(),
                &thumb_path,
                480,
                270,
                0,
            );
            if t != 0 {
                // cropped to the 16:9 box, never squashed: a 4:3 still or a movie's art of any
                // aspect comes back at its own shape (`minSize=1` covers the box, it does not fit it)
                let still = Rect::new(sx, sy, sw, sh);
                let uv = still.cover_uv(tw, th, plx_ui::Crop::Centre);
                p.tex_uv(t, uv, still, 16.0, theme::TINT_WHITE);
                drawn = true;
            }
        }
        if !drawn {
            plx_ui::placeholder::flat(
                p,
                plx_ui::placeholder::Reason::InfoPanelStill,
                &thumb_path,
                Rect::new(sx, sy, sw, sh),
                16.0,
            );
        }

        // action buttons (right column)
        let acts = actions(meta);
        let focus = self.focus;
        let env = plx_ui::Env::inert();
        for (i, label) in acts.iter().enumerate() {
            let icon = if i == 0 {
                Icon::Play
            } else {
                Icon::Info
            };
            if let Ok(cs) = CString::new(*label) {
                plx_ui::widgets::Button::new(cs.as_ptr(), theme::size::BODY, button_rect(i, measure))
                    .icon(icon)
                    .focused(i as c_int == focus)
                    .scale(self.ctl_pop.scale(i))
                    .draw(&env, p);
            }
        }

        // text block (between the still and the buttons): title + synopsis + tags, cap-band centred as a
        // group. Title is the playing leaf's own name (episode name / movie title) — the show-title +
        // SxEy treatment lives on the transport HUD under the playbar, not this card.
        let bx = button_rect(0, measure).x; // the action column's own left edge, every row shares it
        let tx = sx + sw + 34.0;
        let tright = bx - 34.0;
        let tw = tright - tx;
        let white = theme::TEXT_PRIMARY;
        let dim = theme::TEXT_SECONDARY;

        let info_title = c.title.clone();
        let summary = c.summary.clone();
        // metadata line (genres · year · duration) + capability badges: built and FIT here, before the
        // title/synopsis layout below, because the row's actual chip COUNT, `n`, decides whether the
        // group below reserves its line (see [`info_chips`] for issue #26 and the drop rule).
        let chips = info_chips(&c, &FactsShape::CLASSIC, ps, measure);
        let row = chips.row(tx);
        let kept = row.fit(tw);
        let n = row.kept(kept).placed().count();

        // vertical rhythm — line *advances* (deliberately below the full font line-box) + small gaps
        let title_h = 42.0f32; // title advance (font 40)
        // Issue #29: the synopsis is reading copy (the longest run of prose on this card), which is
        // exactly the case `ui/CLAUDE.md` rule 2 already settled — the hero blurb is `size::LABEL` 26
        // through `ui::hero_synopsis`, one rung below plain `size::BODY`, precisely because a blurb
        // reads as prose rather than as chrome. This card built its own synopsis at `BODY` instead of
        // going through that shared helper (it needs its own 2-line cap and card-local ink, not the
        // hero's 3-line one), so it never inherited the correction; stepping it to `LABEL` brings the
        // player's synopsis in line with both heroes' own settled rung. The line advance keeps the same
        // `font + 3` rhythm the card already used at `BODY` (31 = 28 + 3), rather than carrying the old
        // px figure forward onto a smaller font, which would leave the new rung looking loose.
        let syn_lh = theme::size::LABEL as f32 + 3.0; // synopsis line advance (font 26) — 29
        let tag_h = 34.0f32;
        // Off-ladder gaps, kept at their painted values (named, not rounded to a rung).
        let gap_title = 6.0f32; // title → synopsis
        let gap_tags = 12.0f32; // whatever is above → the tag row (title → tags when there is no synopsis)

        // title (1 line, elided) + synopsis (up to 2 lines, ellipsized) through the shared TextView —
        // its wrap is memoised internally, replacing this panel's old hand-rolled wrap2/WrapCache.
        let title_v = TextView::new(&info_title, theme::size::TITLE, white)
            .bold()
            .max_lines(1);
        let syn_v = TextView::new(&summary, theme::size::LABEL, dim)
            .leading(syn_lh)
            .max_lines(2);
        let syn_h = if summary.is_empty() {
            0.0
        } else {
            syn_v.measure_h(tw)
        };

        // centre the [title + synopsis + tag row] group in the card (cap-top coordinates). The tag
        // row's contribution is gated on `n > 0` — whether the chip row ACTUALLY has something to draw
        // — not `has_tags`; see the comment above where `chips`/`n` are built for why those are two
        // different questions once a fit can drop every candidate.
        let items = card_items(title_h, syn_h, gap_tags, (n > 0).then_some(tag_h));
        let stack = VStack::new(&items, gap_title, Anchor::Centre(cyt + ch * 0.5));
        if let Some((y, _)) = stack.span(0) {
            title_v.draw(p, Rect::new(tx, y, tw, 0.0));
        }
        if let Some((y, _)) = stack.span(1) {
            syn_v.draw(p, Rect::new(tx, y, tw, 0.0));
        }
        // metadata line (genres · year · duration) + capability badges, centred on the tag row —
        // `chips`/`n` were already measured and fit above, so this is draw-only.
        //
        // **Issue #26.** This row used to draw every candidate unconditionally and let `mx` run past
        // the action column whenever an item had enough facts — a rating, a premium audio tag, CC, SDH
        // and AD can all be true of one stream at once — so the chips drew straight under "From
        // Beginning"/"Go to Show". The fix constrains the row to `tw`, the SAME right edge the title
        // and synopsis above it already respect, and decides what fits by measuring every candidate
        // BEFORE drawing any of it, in a fixed priority order: the item's own facts (genres/year/
        // duration) first, then the live playback fact (what the server is actually sending — the
        // technical heart of this panel, and the one thing here that can surprise a viewer), then the
        // rating badge, then the premium-audio badge, then the accessibility badges CC/SDH/AD — each
        // rarer, and each a smaller part of why this panel got opened, than the one before it. Dropped
        // chips are hidden outright rather than faded: each one is a discrete fact (a word, a bordered
        // badge), and a badge sliced in half by a fade band reads as a rendering bug, not as "there was
        // more" — a clean drop keeps every chip that IS shown fully legible, which a fade cannot
        // promise once it crosses a border. [`ChipRow`] + `HStack::fit` is the pure packer that turns the
        // measured candidates into a cut, so the maths is pinned by a host test with no font involved.
        if let Some((y, _)) = stack.span(2) {
            let my = y + tag_h * 0.5; // vertical centre of the tag row
            draw_chip_row(p, &chips, chips.row(tx).kept(kept), my, measure);
        }
    }
}

/// The card's title / synopsis / tag-row column. `syn_h` of zero and `tags` of `None` are absent.
/// The tag row carries its gap as `before`, not the stack gap: with no synopsis it still sits
/// `gap_tags` under the title (the stack's own gap is title -> synopsis).
fn card_items(title_h: f32, syn_h: f32, gap_tags: f32, tags: Option<f32>) -> [Item; 3] {
    [
        Item::new(title_h),
        if syn_h > 0.0 { Item::new(syn_h) } else { Item::absent() },
        tags.map_or(Item::absent(), |h| Item::new(h).before(gap_tags)),
    ]
}

/// **The Engine-shaped view of this card** (restructure phase 12): one `Column` focus group over
/// the two action buttons, built fresh by `screens::player::overlay::PlayerOverlayScreen` each
/// frame from a `&InfoPanelState` — the same borrowed-view shape `appkit::more_menu::MoreMenuPart`/
/// `appkit::track_menu::TrackMenuPart` use for the other player panels, so the card answers the same
/// [`Focusable`]/[`Part`] query protocol they do. DOWN off the last button is `EdgeRule::Screen` —
/// re-delivered to the owning screen's own `step`, which drops focus back onto the HUD tabs; UP
/// off the first is `Stop`, matching the old ladder's clamp.
///
/// **`state` is a SHARED reference** — every [`Focusable`] method here is a pure read (`&self`),
/// and the owning screen's own `Focusable` impl only ever has `&self` too (§7.1: "the engine never
/// mutates a screen"), so a mutable field would make this type unconstructable from there. The
/// actual paint (`InfoPanelState::draw`) stays a direct call on the owned `Panel` from
/// `PlayerOverlayScreen::draw`'s `&mut self`; [`Part::draw`] below only registers stops.
pub struct InfoPanelPart<'a> {
    pub state: &'a InfoPanelState,
    pub entry: EntryId,
    pub group: GroupId,
}

impl<H: Host> Focusable<H> for InfoPanelPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Stop, EdgeRule::Screen, EdgeRule::Stop, EdgeRule::Stop],
            extent: card_geometry().0,
            len: N_ACTIONS,
            elem: ElemKind::Control,
        });
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        ((key.index()? as usize) < N_ACTIONS).then_some(self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        let Some(i) = key.elem.index() else {
            return Step::Edge;
        };
        let n = N_ACTIONS as u32;
        match dir {
            Dir::Up if i > 0 => Step::Move(FocusKey { entry: self.entry, elem: H::Elem::of_index(i - 1) }),
            Dir::Down if i + 1 < n => Step::Move(FocusKey { entry: self.entry, elem: H::Elem::of_index(i + 1) }),
            _ => Step::Edge,
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let i = key.index()? as usize;
        if i >= N_ACTIONS {
            return None;
        }
        let r = button_rect(i, cx.measure);
        Some(Placed {
            rect: r,
            rest_rect: r,
            clip: card_geometry().0,
            index: Some(i as u32),
        })
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let i = plx_ui::geom::clamp_plain_index(want.elem.index().unwrap_or(0), N_ACTIONS);
        FocusKey { entry: self.entry, elem: H::Elem::of_index(i as u32) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        FocusKey {
            entry: self.entry,
            elem: H::Elem::of_index(self.state.focus.max(0) as u32),
        }
    }
}

impl<H: Host> Part<H> for InfoPanelPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers each action button's stop (§7.6); the card's own paint happens directly on the
    /// owned `InfoPanelState` from `PlayerOverlayScreen::draw` (see the struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        let p = Painter::root();
        for i in 0..N_ACTIONS {
            let r = button_rect(i, f.measure);
            f.stop(
                p,
                Stop {
                    key: FocusKey {
                        entry: self.entry,
                        elem: H::Elem::of_index(i as u32),
                    },
                    rect: r,
                    rest_rect: r,
                    clip: card_geometry().0,
                    hover: Hover::Focus,
                    activate: Activate::Immediate,
                },
            );
        }
    }
}

/// whether the playing item is an episode (→ "Go to Show") rather than a movie ("Go to Movie").
/// A trailer session does not install a playing-leaf descriptor, so with `now_playing` absent
/// the loaded page is the parent — a show's trailer still says Go to Show.
fn is_episode(meta: metadata::MetadataView<'_>) -> bool {
    meta.now_playing()
        .map(|n| n.is_episode)
        .unwrap_or_else(|| meta.current().is_some_and(|d| d.is_show))
}

/// the action-button labels for the playing item. An ARRAY, not a `Vec`: the count is fixed at
/// [`N_ACTIONS`] (it is what `CtlPop`'s const generic is sized from) — every caller that only
/// needs the count uses that constant instead, so this is called only where the actual label TEXT
/// is drawn ([`InfoPanelState::draw`]).
pub(crate) fn actions(meta: metadata::MetadataView<'_>) -> [&'static str; 2] {
    [
        plx_platform::i18n::msg::widgets_info_from_beginning(),
        if is_episode(meta) {
            plx_platform::i18n::msg::widgets_info_go_show()
        } else {
            plx_platform::i18n::msg::widgets_info_go_movie()
        },
    ]
}

/// The card's own rect and its side padding — **pure**, since every input is a fixed layout
/// constant (`SCR_W`/`SCR_H` alone). Shared by [`InfoPanelState::draw`] (the still/title/synopsis
/// layout) and [`button_rect`] (the action column), so the two formulas can never drift apart —
/// they used to be two copies of the same five numbers, seven lines apart.
pub(crate) fn card_geometry() -> (Rect, f32) {
    let cx = 80.0f32;
    let cw = SCR_W - 160.0;
    let ch = 236.0f32;
    let cyt = SCR_H - 176.0 - ch; // sit just above the Info/Chapters tabs (tabs at SCR_H-128)
    (Rect::new(cx, cyt, cw, ch), 28.0f32)
}

/// The `i`-th action button's rect, widened for the longest translated action label. The same
/// measured formula [`InfoPanelPart::place`] answers the focus engine with, so a stop built from it lands
/// on the pixel the button was drawn at.
pub(crate) fn button_rect(i: usize, measure: &dyn plx_machine::machine::Measure) -> Rect {
    let (card, pad) = card_geometry();
    let labels = [
        plx_platform::i18n::msg::widgets_info_from_beginning_c(),
        plx_platform::i18n::msg::widgets_info_go_show_c(),
        plx_platform::i18n::msg::widgets_info_go_movie_c(),
    ];
    let bw = labels.iter().map(|label| {
        plx_ui::widgets::Button::pill_w_measured(label, theme::size::BODY, true, None, measure)
    }).fold(352.0f32, f32::max);
    let bh = 70.0f32;
    let bx = card.x + card.w - pad - bw;
    let n = N_ACTIONS;
    let total_bh = n as f32 * bh + n.saturating_sub(1) as f32 * 16.0;
    let by0 = card.y + (card.h - total_bh) * 0.5;
    Rect::new(bx, by0 + i as f32 * (bh + 16.0), bw, bh)
}

/// **What the Info section says about the playing item**, resolved once: `now_playing` describes the
/// episode (show title + SxEy + its still) or the movie, the loaded `Detail` backs the genres, and the
/// capability badges come from the PLAYING item's own tracks (`current()` is the show, with episode 1's
/// streams, during a show-page episode play, or another item entirely). The ONE reading of those
/// fields: the hand-written card draws from it, and so does the HUD renderer's section arms.
pub(crate) struct InfoContent<'a> {
    /// the playing leaf's own name: the episode's, else the movie's (the show-title + SxEy treatment
    /// lives on the transport HUD under the playbar, not here)
    pub title: String,
    pub summary: String,
    pub year: i64,
    pub dur_ms: i64,
    pub rating: String,
    /// the 16:9 still (episode) or landscape art (movie)
    pub thumb: String,
    pub genres: &'a [String],
    pub audio: &'a [metadata::Stream],
    pub subs: &'a [metadata::Stream],
}

/// The playing item's [`InfoContent`], or `None` when nothing playing has a name to show.
pub(crate) fn info_content<'a>(meta: metadata::MetadataView<'a>) -> Option<InfoContent<'a>> {
    let np = meta.now_playing();
    let d = meta.current();
    if np.is_none() && d.is_none() {
        return None;
    }
    let is_ep = np.map(|n| n.is_episode).unwrap_or(false);
    let big_title = np.map(|n| n.title.clone()).or_else(|| d.map(|x| x.title.clone())).unwrap_or_default();
    let ep_name = np.map(|n| n.ep_title.clone()).unwrap_or_default();
    let summary = np
        .map(|n| n.summary.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| x.summary.clone()))
        .unwrap_or_default();
    let thumb = np
        .map(|n| n.thumb.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| if !x.art.is_empty() { x.art.clone() } else { x.thumb.clone() }))
        .unwrap_or_default();
    let (audio, subs): (&[metadata::Stream], &[metadata::Stream]) = match meta.playing() {
        Some(t) => (&t.audio, &t.subs),
        None => (d.map(|x| x.audio.as_slice()).unwrap_or(&[]), d.map(|x| x.subs.as_slice()).unwrap_or(&[])),
    };
    Some(InfoContent {
        title: if is_ep { ep_name } else { big_title },
        summary,
        year: np.map(|n| n.year).or_else(|| d.map(|x| x.year)).unwrap_or(0),
        dur_ms: np.map(|n| n.dur_ms).or_else(|| d.map(|x| x.dur_ms)).unwrap_or(0),
        rating: np.map(|n| n.rating.clone()).filter(|s| !s.is_empty()).or_else(|| d.map(|x| x.rating.clone())).unwrap_or_default(),
        thumb,
        genres: d.map(|x| x.genres.as_slice()).unwrap_or(&[]),
        audio,
        subs,
    })
}

/// A package's `facts` cell as the chip builder reads it (`DrawOp::Facts`): which of the item's own
/// facts, in what order, how many genres, whether the live playback fact follows, which badges, in
/// what priority order, and the inks.
pub(crate) struct FactsShape<'a> {
    /// `genres`, `year`, `duration`, in the order they run
    pub order: &'a [&'a str],
    /// `content_rating`, `audio_format`, `cc`, `sdh`, `ad`, in drop-priority order
    pub badges: &'a [&'a str],
    pub genres_max: usize,
    pub playback_fact: bool,
    pub ink: [f32; 4],
    pub bold: i32,
}

/// How many genres the hand-written card's meta line names (the detail page's About card lists them all).
const CLASSIC_GENRES_MAX: usize = 2;

impl FactsShape<'static> {
    /// What the hand-written card always drew: the same shape `builtin:classic`'s `info.facts` cell declares.
    pub(crate) const CLASSIC: Self = Self {
        order: &["genres", "year", "duration"],
        badges: &["content_rating", "audio_format", "cc", "sdh", "ad"],
        genres_max: CLASSIC_GENRES_MAX,
        playback_fact: true,
        ink: theme::TEXT_PRIMARY,
        bold: 1,
    };
}

/// **The metadata line and the capability badges, built and measured** but not yet fit or drawn: the
/// item's own facts as ONE text run (parts a dot apart exactly as the Detail page's identity line
/// spaces them, `widgets::DottedRun`), the live playback fact in its own quieter run (what the server
/// is actually sending, [`playback_now`]: REGULAR weight and tertiary ink, because it is a fact about
/// this session and not a property of the item; no chip and no capsule, a conversion that worked is
/// not a warning), then the badges. Every candidate's width is known before any of them touches the
/// screen, so the caller's `row.fit(tw)` decides what fits ([`ChipRow`]: tail-first drop, issue #26:
/// the chips used to draw straight under the action column).
///
/// The row's actual chip COUNT, not "there might be something to say", is what the caller's layout
/// must reserve a line for: a single candidate wider than the room fits down to nothing.
pub(crate) fn info_chips(
    c: &InfoContent<'_>,
    shape: &FactsShape<'_>,
    ps: &plx_media::route::PlaybackSession,
    measure: &dyn plx_machine::machine::Measure,
) -> ChipRow {
    // What the server is actually sending, straight off the resolved route. `has_url`, not
    // `!url().is_empty()`: the card draws at ~60/s, and `url()` clones the longest string in the app.
    let now_fact = shape.playback_fact.then(|| {
        playback_now(
            plx_media::route::has_url(ps) && !plx_media::route::play_pending(),
            plx_media::route::is_transcoding(ps),
            plx_media::route::is_remux(ps),
            &plx_media::route::stream_vcodec(ps),
        )
    });
    let now_fact = now_fact.flatten();
    let mut chips = ChipRow::new();
    let mut meta = Vec::new();
    for part in shape.order {
        match *part {
            "genres" => meta.extend(c.genres.iter().take(shape.genres_max).cloned()),
            "year" if c.year > 0 => meta.push(c.year.to_string()),
            "duration" if c.dur_ms > 0 => meta.push(plx_ui::fmt::duration(c.dur_ms)),
            _ => {}
        }
    }
    let has_meta = !meta.is_empty();
    if has_meta {
        chips.push(Chip::run(&meta, false, shape.bold, shape.ink, measure));
    }
    // It continues the meta run, so it leads with a dot when there is one. Whether it is needed depends
    // only on whether the meta line EXISTS, not on whether it ends up FITTING: a chip only draws once
    // every higher-priority chip already fit.
    if let Some(fact) = now_fact.as_ref() {
        chips.push(Chip::run(std::slice::from_ref(fact), has_meta, 0, theme::TEXT_TERTIARY, measure));
    }
    for badge in shape.badges {
        match *badge {
            "content_rating" if !c.rating.is_empty() => chips.push(Chip::badge(c.rating.clone(), measure)),
            "audio_format" => {
                if let Some(tag) = c.audio.first().and_then(|s| audio_badge(&s.codec)) {
                    chips.push(Chip::badge(tag, measure));
                }
            }
            "cc" if !c.subs.is_empty() => chips.push(Chip::badge(plx_platform::i18n::msg::widgets_badge_cc().to_string(), measure)),
            "sdh" if c.subs.iter().any(|s| s.sdh) => chips.push(Chip::badge(plx_platform::i18n::msg::widgets_badge_sdh().to_string(), measure)),
            "ad" if c.audio.iter().any(|s| s.ad) => chips.push(Chip::badge(plx_platform::i18n::msg::widgets_badge_ad().to_string(), measure)),
            _ => {}
        }
    }
    chips
}

// ---- helpers ----

/// A premium audio format worth badging on the meta line, named by the ONE codec map
/// ([`metadata::friendly_codec`]); everyday codecs (AAC/MP3/…) get no badge.
fn audio_badge(codec: &str) -> Option<String> {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "truehd" | "eac3" | "ec-3" | "ac3" | "dts" | "dca"
    )
    .then(|| metadata::friendly_codec(codec))
}

/// A VIDEO codec's display name. The sibling of [`metadata::friendly_codec`], which is the one
/// AUDIO/subtitle map and spells h264 as "H264" — the dotted form is the one everybody else's
/// player uses for the video lane, and this is the only place the app names a video codec to a
/// user. An unknown codec is upper-cased rather than dropped: "Converting · AV1" is still the
/// truth on a set that cannot decode AV1 (the dev set, and every set whose codec table lists no AV1
/// decoder), and inventing a friendly name for a codec we have never seen would not be.
pub fn video_codec_name(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" => "H.264".to_string(),
        "hevc" | "h265" | "hvc1" | "hev1" => "HEVC".to_string(),
        other => other.to_uppercase(),
    }
}

/// The live playback fact for the meta line: **what the server is actually sending**, read off the
/// running stream and never predicted.
///
/// That distinction is the whole design of this line. Everything else on the row is a property of
/// the ITEM (genre, year, runtime), knowable before Play; this is a property of THIS SESSION, and
/// the only honest source for it is `route`'s own record of what was resolved — `stream_vcodec` is
/// the codec the `/decision` OUTPUT named, not the one the profile asked for. Predicting it from
/// the source file is how a client ends up telling the user it is direct playing a file the server
/// quietly re-encoded.
///
/// Three answers, because the server has three behaviours and only one of them touches the pixels:
/// a direct play (we pull the file), a container-only REMUX (the video is always copied and the
/// audio is copied when the profile admits it, converted otherwise — Plex's own "Direct Stream";
/// the mock has no case for it because its model has only "direct play" and "converts", but
/// calling a remux a conversion would state that the server re-encoded the pixels when it did
/// not), and a real re-encode, which names the codec it is producing. Hardware vs software is
/// deliberately NOT stated: that is the server's runtime choice and it reaches the client only in
/// the live transcode session, so naming it would be a guess.
///
/// **No warning belongs on this line.** The tone-mapping case in particular lives on the DETAIL
/// page, before Play: this card is behind a keypress, so a warning here is one the user finds only
/// after watching a washed-out picture. That is `Player Screen.dc.html`'s own standing comment
/// ("Warning before Play is the DETAIL page's job"), and it contradicts a bullet in the **design
/// project's** `CLAUDE.md` — the Pass-rules file in the Claude Design project, NOT this repo's root
/// `CLAUDE.md`, which says nothing about tone mapping — that lists this card beside the detail
/// facts row. The mock is the later artifact and its reasoning matches the owner's standing rule
/// that a warning exists to be seen BEFORE Play, so the code follows it; the two documents still
/// need reconciling by the owner, and that is a docs edit, not a behaviour change.
///
/// PURE, so every arm is host-testable. `streaming` is "there is a resolved stream to describe".
/// The caller composes it from `route::has_url()` **and** `!route::play_pending()`, and the second
/// half is load-bearing rather than belt-and-braces: `request_play` does not clear `URL`,
/// `TSESSION` or `CUR_REMUX` (only a real stop does, in `engine::teardown`), so on an item→item
/// switch — Up Next, or Play from a detail page while a session is live — every one of those three
/// still describes the PREVIOUS item for the whole resolve. Without the pending test this line
/// would state the last session's fact as this one's.
pub fn playback_now(
    streaming: bool,
    transcoding: bool,
    remux: bool,
    vcodec: &str,
) -> Option<String> {
    if !streaming {
        return None;
    }
    if !transcoding {
        return Some(plx_platform::i18n::msg::widgets_playback_direct_play().to_string());
    }
    if remux {
        return Some(plx_platform::i18n::msg::widgets_playback_direct_stream().to_string());
    }
    let name = video_codec_name(vcodec);
    // a re-encode whose output codec we somehow do not know still converted — say that much
    Some(if name.is_empty() {
        plx_platform::i18n::msg::widgets_playback_converting().to_string()
    } else {
        plx_platform::i18n::msg::widgets_playback_converting_codec(&name)
    })
}

#[cfg(test)]
mod tests {
    use super::playback_now;

    /// The meta line's live fact, arm by arm. It is the one thing on that row the app could get
    /// wrong by GUESSING, so each arm pins a different way of guessing:
    ///   * a direct play says so and names no codec — there is no conversion to describe;
    ///   * a container remux is NOT a conversion (the video is copied, and the audio too when the
    ///     profile admits it), and calling it one would state that the server touched the pixels
    ///     when it did not — `route::is_remux`'s own doc
    ///     is that these are different facts;
    ///   * a re-encode names the codec the server is actually PRODUCING (`route::stream_vcodec` is
    ///     the `/decision` OUTPUT), spelled the way a viewer reads it, and HEVC output is reachable
    ///     only on a Pass server — which this function neither knows nor asks, because it reports
    ///     what happened rather than what was allowed;
    ///   * and with no stream resolved there is no fact at all: during the resolve window nothing
    ///     has been sent yet, and stating a fact about it would be exactly the prediction the whole
    ///     line refuses to make.
    #[test]
    fn the_meta_lines_playback_fact_describes_the_stream_that_is_running() {
        assert_eq!(
            playback_now(true, false, false, "hevc").as_deref(),
            Some("Direct Play")
        );
        // remux: `vcodec` is the SOURCE codec copied through, and it is still not a conversion
        assert_eq!(
            playback_now(true, true, true, "hevc").as_deref(),
            Some("Direct Stream")
        );
        assert_eq!(
            playback_now(true, true, false, "h264").as_deref(),
            Some("Converting \u{b7} H.264")
        );
        assert_eq!(
            playback_now(true, true, false, "hevc").as_deref(),
            Some("Converting \u{b7} HEVC")
        );
        // a codec map we have never met is upper-cased, not dropped and not renamed
        assert_eq!(
            playback_now(true, true, false, "av1").as_deref(),
            Some("Converting \u{b7} AV1")
        );
        // a re-encode whose output codec never landed still converted
        assert_eq!(
            playback_now(true, true, false, "").as_deref(),
            Some("Converting")
        );
        // nothing resolved yet → no fact, on every combination of the flags
        for (t, r) in [(false, false), (true, false), (true, true)] {
            assert!(
                playback_now(false, t, r, "h264").is_none(),
                "no stream, no fact"
            );
        }
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};

    // TEST ONLY: a thread-confined store, so `set_current_for_test`/`apply` and the `view()`
    // this test's `on_ok`/`is_episode` calls read from are the SAME owner, not two disconnected
    // `MetadataStore::default()`s (same pattern as `screens::detail::tests`'s `TEST_METADATA`).
    thread_local! {
        static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
            std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
    }

    fn test_store() -> &'static mut plx_data::stores::metadata::MetadataStore {
        TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
    }

    struct HostFixture;
    impl Host for HostFixture {
        type Arg = plx_ui::fixture::FixtureArg;
        type Fx = plx_ui::fixture::FixtureFx;
        type Msg = plx_ui::fixture::FixtureMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = plx_ui::fixture::FixtureInit;
        type Memory = ();
    }

    fn with_cx<R>(entry: EntryId, test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let measure = plx_ui::fixture::FixtureMeasure;
        test(&Cx {
            views: (),
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead::default(),
            press: PressRead::default(),
            owner: InputOwner::Entry(entry),
        })
    }

    /// **UP/DOWN step by one button and clamp at both ends** over the fixed two-button column.
    #[test]
    fn up_down_step_by_one_and_clamp_at_both_ends() {
        let e = EntryId(6);
        let st = InfoPanelState::new();
        let part = InfoPanelPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let step = |i: u32, dir: Dir| {
                match <InfoPanelPart as Focusable<HostFixture>>::neighbour(
                    &part,
                    FocusKey { entry: e, elem: i },
                    dir,
                    cx,
                ) {
                    Step::Move(k) => Some(k.elem),
                    Step::Edge => None,
                }
            };
            assert_eq!(step(0, Dir::Down), Some(1));
            assert_eq!(step(1, Dir::Down), None, "the last button is the screen's own edge");
            assert_eq!(step(0, Dir::Up), None, "the first button does not wrap");
        });
    }

    /// `place` reports exactly the rect [`InfoPanelState::draw`] paints the button at.
    #[test]
    fn place_matches_the_draw_formula() {
        let e = EntryId(6);
        let st = InfoPanelState::new();
        let part = InfoPanelPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            for i in 0..N_ACTIONS as u32 {
                let placed = <InfoPanelPart as Focusable<HostFixture>>::place(&part, &i, cx, At::Drawn)
                    .expect("both buttons are placeable");
                let want = button_rect(i as usize, cx.measure);
                assert_eq!(
                    (placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h),
                    (want.x, want.y, want.w, want.h)
                );
            }
        });
    }

    /// Two buttons stack vertically, sharing an x and a width, in the order `draw` paints them —
    /// "From Beginning" above "Go to Show"/"Go to Movie".
    #[test]
    fn the_two_buttons_share_a_column_and_stack_in_draw_order() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let (a, b) = (button_rect(0, &measure), button_rect(1, &measure));
        assert_eq!(a.x, b.x);
        assert_eq!(a.w, b.w);
        assert!(a.y < b.y, "From Beginning sits above the second action");
    }

    /// DOWN off the last button is the SCREEN's edge (`FocusTabs`), never `Stop` — the group's
    /// declared edges pin the direction each rule below answers.
    #[test]
    fn down_off_the_last_button_is_the_screens_edge() {
        let e = EntryId(6);
        let st = InfoPanelState::new();
        let part = InfoPanelPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let mut groups = Vec::new();
            <InfoPanelPart as Focusable<HostFixture>>::groups(&part, cx, &mut groups);
            let g = groups.into_iter().next().expect("one group");
            assert!(matches!(g.edge[1], EdgeRule::Screen), "down");
            assert!(matches!(g.edge[0], EdgeRule::Stop), "up");
        });
    }

    #[test]
    fn go_to_after_a_trailer_opens_the_loaded_parent() {
        let _g = plx_base::testlock::serial();
        test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(None));
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            rk: "parent-movie".into(),
            kind: "movie".into(),
            ..Default::default()
        }));
        let mut movie = InfoPanelState::new();
        movie.set_focus(1);
        assert_eq!(
            movie.on_ok(test_store().view()),
            InfoAction::GoToDetail("parent-movie".into())
        );
        assert!(!is_episode(test_store().view()), "a movie parent labels Go to Movie");

        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            rk: "parent-show".into(),
            kind: "show".into(),
            is_show: true,
            ..Default::default()
        }));
        let mut show = InfoPanelState::new();
        show.set_focus(1);
        assert_eq!(show.on_ok(test_store().view()), InfoAction::GoToDetail("parent-show".into()));
        assert!(is_episode(test_store().view()), "a show parent labels Go to Show");

        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            sid: plx_plex::plex::ServerId::UNSET,
            rk: "parent-show".into(),
            kind: "show".into(),
            is_show: true,
            title: "Show".into(),
            extras: vec![plx_data::metadata::Extra {
                rk: "9".into(),
                title: "Official Trailer".into(),
                dur_ms: 120_000,
                part: "/p".into(),
                ..Default::default()
            }],
            ..Default::default()
        }));
        let trailer = plx_data::metadata::trailer_now_playing(test_store().state(), plx_plex::plex::ServerId::UNSET, "9");
        test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(trailer));
        let mut playing = InfoPanelState::new();
        playing.set_focus(1);
        assert_eq!(playing.on_ok(test_store().view()), InfoAction::GoToDetail("parent-show".into()));
        assert!(is_episode(test_store().view()), "an installed show-trailer card still says Go to Show");
        assert_eq!(
            test_store().view().now_playing().map(|n| n.dur_ms),
            Some(120_000),
            "the extra's duration, not the show's"
        );

        test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(None));
        plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    }
}

#[cfg(test)]
mod vstack_tests {
    use super::*;

    fn ys(syn_h: f32, tags: Option<f32>) -> Vec<(usize, f32)> {
        let items = card_items(42.0, syn_h, 12.0, tags);
        VStack::new(&items, 6.0, Anchor::Top(0.0)).placed().map(|s| (s.i, s.y)).collect()
    }

    /// Title, synopsis, tags: 6 then 12 apart; with no synopsis the tags stay 12 under the title, not 6.
    #[test]
    fn the_gaps_survive_an_absent_synopsis() {
        assert_eq!(ys(58.0, Some(34.0)), vec![(0, 0.0), (1, 48.0), (2, 118.0)]);
        assert_eq!(ys(0.0, Some(34.0)), vec![(0, 0.0), (2, 54.0)]);
        assert_eq!(ys(58.0, None), vec![(0, 0.0), (1, 48.0)]);
    }
}
