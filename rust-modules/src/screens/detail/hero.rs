//! Detail hero policy: dynamic action controls, people credits, playback facts and their exact
//! width/priority rules. Painting orchestration stays in [`super::DetailScreen`]; the decisions
//! here are explicit-value helpers so host tests need no global focus state.
//!
//! Control selection and geometry take explicit `Detail`/`HeroSet` values. The playback-facts
//! helpers also consult cached application policy and the text renderer; they are not all pure
//! host-only functions. The screen owns the metadata identity and supplies the current item.
//!
//! These helpers are orchestrated by the completed screen's direct `Focusable`/`Screen` impls;
//! they do not own an independent focus cursor or screen lifetime.
//!
//! # The `ElemKey` numbering convention this package establishes (spec §13's own instruction:
//! "reserve a range so the next package can pick a disjoint one")
//!
//! Hero controls occupy the `u32` elem range **`[0, HERO_ELEM_RANGE_END)` = `[0, 64)`**. Every
//! constant below is an IDENTITY, not a POSITION — `ELEM_MARK_WATCHED`/`ELEM_MARK_UNWATCHED` are
//! two DIFFERENT ids for the two faces one control wears, never both present at once, which is
//! what lets [`DetailScreen::reconcile`](super::DetailScreen) carry the focus across a press that
//! flips the toggle (see that impl's doc — the engine calls `reconcile` with whatever key focus
//! currently holds, every frame, so identity-as-elem plus a membership test reproduces
//! the legacy `hero_col` correction without a second cursor). Section helpers use disjoint LOCAL
//! slot ranges starting at [`HERO_ELEM_RANGE_END`]; `DetailScreen` projects repeated items to stable
//! engine identities instead of exposing those positional slots as focus keys.
use std::ffi::{CStr, CString};
use std::os::raw::c_int;

use crate::metadata::{Detail, Episode};
use crate::ui::label::HAlign;
use crate::ui::machine::{GroupId, Measure};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{CircleButton, PosterMark};
use crate::ui::{theme, Painter, Rect};

/// The hero's whole `u32` elem namespace. A later Detail package's own range must start here.
pub(crate) const HERO_ELEM_RANGE_END: u32 = 64;

pub(crate) const ELEM_PLAY: u32 = 0;
pub(crate) const ELEM_RESTART: u32 = 1;
pub(crate) const ELEM_ALT: u32 = 2;
pub(crate) const ELEM_MARK_WATCHED: u32 = 3;
pub(crate) const ELEM_MARK_UNWATCHED: u32 = 4;

/// The hero's one focus group. Local to this screen — nothing outside `screens::detail` ever names
/// it — so, like `screens::profiles`'s `ROSTER_GROUP`/`FOOTER_GROUP`, any small integer would do;
/// `0` matches this family's own convention of seating group `GroupId(0)` as a screen's primary
/// group.
pub(crate) const HERO_GROUP: GroupId = GroupId(0);

/// The Play/Resume pill's minimum width — a pathologically short label still gets a pill.
const PW: f32 = 168.0;

/// The row's inter-control air and its disc diameter — the SHARED control-family numbers
/// (`ui::widgets::CTRL_GAP`/`StatusOverlay::CTRL_H`), not a second copy of them.
const CGAP: f32 = crate::ui::widgets::CTRL_GAP;
pub(crate) const CD: f32 = crate::ui::widgets::StatusOverlay::CTRL_H;
const PEOPLE_MAX_LINES: usize = 2;
const PEOPLE_CAST: usize = 3;
const PEOPLE_LABEL_INK: [f32; 4] = theme::TEXT_TERTIARY;
const FACTS_SEP_PAD: f32 = theme::space::SM;
const FACTS_R: f32 = crate::ui::consts::SCR_W
    - crate::ui::consts::MARGIN_X
    - crate::ui::detail_layout::PEOPLE_W
    - theme::space::SM;

/// Duplicated from `widgets::Button`'s own PRIVATE `BTN_ICON_RATIO`/`BTN_ICON_GAP` (1.15, 12.0):
/// those two constants are `widgets.rs`-internal, and this module may not reach past that file's
/// `pub`/`pub(crate)` surface (the layer rule: a screen names `ui/`'s public surface, never its
/// internals). `BTN_PILL_AIR` IS `pub(crate)` and is reused directly rather than copied.
///
/// A drift here is COSMETIC, not a hit-test miss: whatever width this formula computes is fed to
/// BOTH [`Focusable::place`](super::DetailScreen) and `draw`'s `Button::new(label, sz, rect)` (which
/// only ever fills the rect it is given — see `widgets::Button::new`'s doc), so the two paths can
/// disagree with the REAL widget's own preferred width, never with EACH OTHER.
const HERO_ICON_RATIO: f32 = 1.15;
const HERO_ICON_GAP: f32 = 12.0;

pub(crate) const ALT_LABEL: &CStr = c"Also available";

const MARK_WATCHED_LABEL: &CStr = c"Mark as Watched";
const MARK_UNWATCHED_LABEL: &CStr = c"Mark as Unwatched";
const MARK_SHOW_WATCHED_LABEL: &CStr = c"Mark Show as Watched";
const MARK_SHOW_UNWATCHED_LABEL: &CStr = c"Mark Show as Unwatched";
const PLAY_FROM_START_LABEL: &CStr = c"Play from Start";

/// A control in the hero action row, named rather than numbered — ported verbatim from
/// `ui/detail.rs::HeroCtl`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HeroCtl {
    /// the Play/Resume pill — always present, always first
    Play,
    /// the ↺ disc, present only while there is a resume point for it to ignore
    Restart,
    /// the *Also available* pill, present only while a second pinned source holds this item
    Alt,
    /// the ✓ face of the watched TOGGLE — worn while the item is not watched (part-watched
    /// included)
    MarkWatched,
    /// the − face of that same toggle — worn once the item IS watched
    MarkUnwatched,
}

impl HeroCtl {
    /// This control's fixed `u32` identity (never its position — see the module doc).
    pub(crate) const fn elem(self) -> u32 {
        match self {
            HeroCtl::Play => ELEM_PLAY,
            HeroCtl::Restart => ELEM_RESTART,
            HeroCtl::Alt => ELEM_ALT,
            HeroCtl::MarkWatched => ELEM_MARK_WATCHED,
            HeroCtl::MarkUnwatched => ELEM_MARK_UNWATCHED,
        }
    }
    /// The inverse of [`elem`](Self::elem) — `None` for any `u32` outside the five identities
    /// above, which is every elem a NON-hero group can mint (the completed sections use
    /// their own disjoint range, per the module doc).
    pub(crate) fn of_elem(e: u32) -> Option<Self> {
        match e {
            ELEM_PLAY => Some(HeroCtl::Play),
            ELEM_RESTART => Some(HeroCtl::Restart),
            ELEM_ALT => Some(HeroCtl::Alt),
            ELEM_MARK_WATCHED => Some(HeroCtl::MarkWatched),
            ELEM_MARK_UNWATCHED => Some(HeroCtl::MarkUnwatched),
            _ => None,
        }
    }
    /// Is `self` the watched toggle, wearing either face?
    pub(crate) fn is_watch(self) -> bool {
        matches!(self, HeroCtl::MarkWatched | HeroCtl::MarkUnwatched)
    }
}

/// Which of the conditional controls the row is showing: the two independent bits, plus the
/// item's watch state, which decides which FACE the watched toggle wears. Ported verbatim from
/// `ui/detail.rs::HeroSet`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct HeroSet {
    pub(crate) restart: bool,
    pub(crate) alt: bool,
    pub(crate) mark: PosterMark,
}

/// The row's controls, in drawn order, for a given set — ported verbatim from
/// `ui/detail.rs::hero_ctls`. A fixed 4-slot array (Play + Restart + Alt + one watch face is the
/// widest the row ever gets) plus a live count, so no per-frame allocation.
pub(crate) fn hero_ctls(set: HeroSet) -> ([HeroCtl; 4], usize) {
    let mut v = [HeroCtl::Play; 4];
    let mut n = 1;
    if set.restart {
        v[n] = HeroCtl::Restart;
        n += 1;
    }
    if set.alt {
        v[n] = HeroCtl::Alt;
        n += 1;
    }
    v[n] = if set.mark == PosterMark::Watched {
        HeroCtl::MarkUnwatched
    } else {
        HeroCtl::MarkWatched
    };
    n += 1;
    (v, n)
}

/// The control at index `i` in `set`, or `None` for an index the set does not have.
pub(crate) fn ctl_at(set: HeroSet, i: usize) -> Option<HeroCtl> {
    let (v, n) = hero_ctls(set);
    (i < n).then(|| v[i])
}

/// Where `ctl` sits in `set`, or `None` when the set does not include it.
pub(crate) fn index_of(set: HeroSet, ctl: HeroCtl) -> Option<usize> {
    let (v, n) = hero_ctls(set);
    v[..n].iter().position(|&c| c == ctl)
}

/// Where the watched toggle sits in `set` — always present, so `Some` for every set the row can
/// be in. Keyed on the control's JOB rather than either of its two faces, which is what lets
/// [`DetailScreen::reconcile`](super::DetailScreen) carry the focus across a press that flips it.
pub(crate) fn watch_index(set: HeroSet) -> Option<usize> {
    let (v, n) = hero_ctls(set);
    v[..n].iter().position(|&c| c.is_watch())
}

/// Has this show been started AT ALL? PMS puts `S1E1 off=0` on deck for a show nobody has
/// touched, so "there is something on deck" is not the same question as "this show is underway" —
/// ported verbatim from `ui/detail.rs::show_started`.
pub(crate) fn show_started(d: &Detail) -> bool {
    d.on_deck.as_ref().map(|e| e.resume_ms > 0).unwrap_or(false)
        || d.seasons.iter().any(|s| s.viewed_leaf_count > 0)
}

/// The episode a show's hero is ABOUT — the server's on-deck episode, once the show has actually
/// been started. `None` means the hero presents the SERIES instead (finished, or never opened).
/// Ported verbatim from `ui/detail.rs::hero_episode`.
pub(crate) fn hero_episode(d: &Detail) -> Option<&Episode> {
    let ep = d.on_deck.as_ref()?;
    show_started(d).then_some(ep)
}

/// The resume position (ns) the hero's Play control would ACTUALLY apply. Ported verbatim from
/// `ui/detail.rs::hero_resume_ns`.
pub(crate) fn hero_resume_ns(d: &Detail) -> i64 {
    if d.is_show {
        hero_episode(d)
            .map(|e| crate::metadata::resume_ns(e.resume_ms, e.dur_ms))
            .unwrap_or(0)
    } else {
        crate::metadata::resume_ns(d.resume_ms, d.dur_ms)
    }
}

/// Is there a resume point for the restart disc to ignore?
pub(crate) fn has_restart(resume_ns: i64) -> bool {
    resume_ns > 0
}

/// The loaded item's watch state, in the app's one watch-state vocabulary. Ported verbatim from
/// `ui/detail.rs::hero_watch_state`.
pub(crate) fn hero_watch_state(
    is_show: bool,
    show_started: bool,
    watched: bool,
    resume_ns: i64,
) -> PosterMark {
    let mid_run = is_show && show_started && !watched;
    if resume_ns > 0 || mid_run {
        PosterMark::InProgress
    } else if watched {
        PosterMark::Watched
    } else {
        PosterMark::None
    }
}

/// [`hero_watch_state`] for `d`. Ported verbatim from `ui/detail.rs::hero_mark`.
pub(crate) fn hero_mark(d: &Detail) -> PosterMark {
    hero_watch_state(d.is_show, show_started(d), d.watched, hero_resume_ns(d))
}

/// Does the toggle's subject differ from the one the rest of the hero is about — true exactly
/// while the hero is showing an on-deck EPISODE. Ported verbatim from
/// `ui/detail.rs::watch_names_show`.
pub(crate) fn watch_names_show(d: &Detail) -> bool {
    hero_episode(d).is_some()
}

/// A disc's slot (`[restart, watch]`) and the verb it unfurls to — `None` for the two PILLS.
/// Ported verbatim from `ui/detail.rs::disc_verb`.
pub(crate) fn disc_verb(ctl: HeroCtl, name_show: bool) -> Option<(usize, &'static CStr)> {
    match (ctl, name_show) {
        (HeroCtl::Restart, _) => Some((0, PLAY_FROM_START_LABEL)),
        (HeroCtl::MarkWatched, false) => Some((1, MARK_WATCHED_LABEL)),
        (HeroCtl::MarkWatched, true) => Some((1, MARK_SHOW_WATCHED_LABEL)),
        (HeroCtl::MarkUnwatched, false) => Some((1, MARK_UNWATCHED_LABEL)),
        (HeroCtl::MarkUnwatched, true) => Some((1, MARK_SHOW_UNWATCHED_LABEL)),
        _ => None,
    }
}

/// The Play pill's label — the word the press will actually perform.
pub(crate) fn hero_pill_label(has_restart: bool) -> &'static CStr {
    if has_restart {
        c"Resume"
    } else {
        c"Play"
    }
}

/// `widgets::Button::pill_w_full`, reproduced over [`Measure`] rather than `crate::text` directly
/// — see [`HERO_ICON_RATIO`]'s doc for why the icon-box constants are duplicated, and
/// `screens::login::status_action_rect`'s own doc for the general pattern (host-test-safe geometry
/// shared by `Focusable::place` and `draw`).
fn pill_w(measure: &dyn Measure, label: &CStr, sz: i32, icon: bool, trailing: bool) -> f32 {
    let (isz, gap) = if icon {
        (sz as f32 * HERO_ICON_RATIO, HERO_ICON_GAP)
    } else {
        (0.0, 0.0)
    };
    let (tsz, tgap) = if trailing {
        (sz as f32 * HERO_ICON_RATIO, HERO_ICON_GAP)
    } else {
        (0.0, 0.0)
    };
    isz + gap + measure.width(label, sz, true) + tgap + tsz + crate::ui::widgets::BTN_PILL_AIR
}

pub(crate) fn hero_pill_w(measure: &dyn Measure, has_restart: bool) -> f32 {
    pill_w(
        measure,
        hero_pill_label(has_restart),
        theme::size::BODY,
        true,
        false,
    )
    .max(PW)
}

pub(crate) fn alt_pill_w(measure: &dyn Measure) -> f32 {
    pill_w(measure, ALT_LABEL, theme::size::BODY, false, true)
}

/// Every measured width the row's accumulation needs, as one value — ported verbatim from
/// `ui/detail.rs::HeroWidths`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HeroWidths {
    pub(crate) pill: f32,
    pub(crate) alt: f32,
    /// the two discs, in [`disc_verb`]'s slot order, each already unfurled
    pub(crate) disc: [f32; 2],
}

/// The accumulation itself, PURE: the drawn frame of control `i` in `set`, given both pills'
/// measured widths. Ported verbatim from `ui/detail.rs::hero_btn_rect_at`.
pub(crate) fn hero_btn_rect_at(set: HeroSet, i: usize, y: f32, cw: HeroWidths) -> Rect {
    let (v, n) = hero_ctls(set);
    let mut x = crate::ui::consts::MARGIN_X;
    let mut w = cw.pill;
    for (k, c) in v[..n].iter().enumerate() {
        w = match c {
            HeroCtl::Play => cw.pill,
            HeroCtl::Alt => cw.alt,
            HeroCtl::Restart => cw.disc[0],
            HeroCtl::MarkWatched | HeroCtl::MarkUnwatched => cw.disc[1],
        };
        if k >= i {
            break;
        }
        x += w + CGAP;
    }
    Rect::new(x, y, w, CD)
}

/// Both discs' drawn widths right now, given the row's set, both pills' widths, each disc's
/// unfurl `e` (0..1) and each verb's measured width.
///
/// The widest verb must fit wholly before the People column or both discs remain circles. During
/// a hand-off one closes while the other opens, so budgeting the pair by the wider extra is the
/// exact legacy rule and prevents a half-word crossing into the right-hand copy.
pub(crate) fn disc_caps(
    measure: &dyn Measure,
    set: HeroSet,
    unfurl: [f32; 2],
    named_show: bool,
) -> [f32; 2] {
    let (v, n) = hero_ctls(set);
    let mut label_w = [0.0f32; 2];
    for &c in &v[..n] {
        if let Some((slot, label)) = disc_verb(c, named_show) {
            label_w[slot] = measure.width(label, theme::size::BODY, true);
        }
    }
    disc_caps_at(
        set,
        hero_pill_w(measure, set.restart),
        alt_pill_w(measure),
        unfurl,
        label_w,
    )
}

pub(super) fn disc_caps_at(
    set: HeroSet,
    pill: f32,
    alt: f32,
    unfurl: [f32; 2],
    label_w: [f32; 2],
) -> [f32; 2] {
    let (_, n) = hero_ctls(set);
    let closed = HeroWidths {
        pill,
        alt,
        disc: [CD; 2],
    };
    let last = hero_btn_rect_at(set, n.saturating_sub(1), 0.0, closed);
    let budget = CircleButton::label_budget(CD, FACTS_R - (last.x + last.w));
    if label_w[0].max(label_w[1]) > budget {
        return [CD; 2];
    }
    [
        CircleButton::cap_w(CD, unfurl[0], label_w[0]),
        CircleButton::cap_w(CD, unfurl[1], label_w[1]),
    ]
}

/// The live widths, combining [`hero_pill_w`]/[`alt_pill_w`]/[`disc_caps`] — the one function
/// `DetailScreen`'s `Focusable` queries and `draw` both call, so they can never drift (mirrors
/// `ui/detail.rs::hero_widths`, over `Measure` instead of `crate::text` directly).
pub(crate) fn hero_widths(
    measure: &dyn Measure,
    set: HeroSet,
    has_restart: bool,
    unfurl: [f32; 2],
    named_show: bool,
) -> HeroWidths {
    HeroWidths {
        pill: hero_pill_w(measure, has_restart),
        alt: alt_pill_w(measure),
        disc: disc_caps(measure, set, unfurl, named_show),
    }
}

pub(crate) fn hero_credit(d: &Detail) -> Option<(&'static str, Vec<&str>)> {
    if d.is_show {
        let names: Vec<&str> = d
            .crew
            .iter()
            .filter(|credit| credit.role.contains("Writer"))
            .map(|credit| credit.tag.as_str())
            .collect();
        return (!names.is_empty()).then_some(("Created by", names));
    }
    let names: Vec<&str> = d.directors.iter().map(String::as_str).collect();
    (!names.is_empty()).then_some(("Directed by", names))
}

pub(crate) fn has_people(d: &Detail) -> bool {
    !d.cast.is_empty() || hero_credit(d).is_some()
}

pub(crate) fn draw_people(p: Painter, d: &Detail, button_y: f32, measure: &dyn Measure) {
    use crate::ui::detail_layout::PEOPLE_W;
    let x = crate::ui::consts::SCR_W - crate::ui::consts::MARGIN_X - PEOPLE_W;
    let mut bottom = button_y + CD;
    if !d.cast.is_empty() {
        let names: Vec<&str> = d
            .cast
            .iter()
            .take(PEOPLE_CAST)
            .map(|credit| credit.tag.as_str())
            .collect();
        bottom -= people_line(p, "Starring", &names, x, bottom, measure);
    }
    if let Some((label, names)) = hero_credit(d) {
        people_line(p, label, &names, x, bottom, measure);
    }
}

fn people_line(p: Painter, label: &str, names: &[&str], x: f32, bottom: f32, measure: &dyn Measure) -> f32 {
    use crate::ui::detail_layout::{PEOPLE_INK, PEOPLE_LEAD, PEOPLE_W};
    let names = names
        .iter()
        .map(|name| name.replace(' ', "\u{a0}"))
        .collect::<Vec<_>>()
        .join(", ");
    let view = TextView::new(&names, theme::size::CAPTION, PEOPLE_INK).with_measure(measure)
        .leading(PEOPLE_LEAD)
        .max_lines(PEOPLE_MAX_LINES)
        .h(HAlign::Right)
        .lead_quiet(label, PEOPLE_LABEL_INK);
    let h = view.measure_h(PEOPLE_W);
    view.draw(p, Rect::new(x, bottom - h, PEOPLE_W, 0.0));
    h
}

fn hero_facts(d: &Detail) -> (String, Option<String>) {
    if let Some(ep) = hero_episode(d) {
        let runtime = (ep.dur_ms >= 60_000).then(|| crate::ui::fmt::dur_long(ep.dur_ms));
        return (crate::ui::fmt::pretty_date(&ep.aired, 0), runtime);
    }
    let date = crate::ui::fmt::pretty_date(&d.aired, d.year);
    if d.is_show {
        let seasons = d.seasons.len();
        if seasons == 0 {
            return (date, None);
        }
        let episodes: i64 = d.seasons.iter().map(|season| season.leaf_count).sum();
        let season_word = if seasons == 1 { "season" } else { "seasons" };
        let extent = if episodes > 0 {
            let episode_word = if episodes == 1 { "episode" } else { "episodes" };
            format!("{seasons} {season_word}, {episodes} {episode_word}")
        } else {
            format!("{seasons} {season_word}")
        };
        return (date, Some(extent));
    }
    (
        date,
        (d.dur_ms >= 60_000).then(|| crate::ui::fmt::dur_long(d.dur_ms)),
    )
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PlayNote {
    Quiet,
    Soft,
    Warn,
}

fn play_note(
    preview: crate::route::Preview,
    hdr: bool,
    subscription: crate::plex::serverinfo::Subscription,
) -> PlayNote {
    let converts = preview == crate::route::Preview::Converts;
    let no_pass = subscription == crate::plex::serverinfo::Subscription::No;
    if converts && no_pass && hdr {
        PlayNote::Warn
    } else if converts && no_pass {
        PlayNote::Soft
    } else {
        PlayNote::Quiet
    }
}

fn item_subscription(d: &Detail) -> crate::plex::serverinfo::Subscription {
    crate::plex::serverinfo::subscription_of(d.sid)
}

const FACTS_GLYPH_D: f32 = theme::size::CAPTION as f32;
const CONVERTS_ON_SERVER_C: &CStr = c"Converts on server";

#[derive(Clone, Copy)]
enum Bit {
    Word(&'static CStr, [f32; 4], c_int),
    Sep(f32),
    Glyph,
    Air(f32),
    Capsule,
}

const FACTS_BITS: usize = 8;

fn play_mode_bits(d: &Detail, after: bool) -> ([Bit; FACTS_BITS], usize) {
    let mut bits = [Bit::Air(0.0); FACTS_BITS];
    let mut n = 0;
    let Some(preview) = crate::route::playback_preview(d) else {
        return (bits, 0);
    };
    let mut push = |bit: Bit| {
        bits[n] = bit;
        n += 1;
    };
    if after {
        push(Bit::Sep(theme::space::MD));
    }
    match play_note(preview, d.hdr, item_subscription(d)) {
        PlayNote::Quiet => push(Bit::Word(
            match preview {
                crate::route::Preview::DirectPlay => c"Direct Play",
                crate::route::Preview::Remux => c"Direct Stream",
                crate::route::Preview::Converts => CONVERTS_ON_SERVER_C,
            },
            theme::TEXT_TERTIARY,
            0,
        )),
        PlayNote::Soft => {
            push(Bit::Word(CONVERTS_ON_SERVER_C, theme::TEXT_TERTIARY, 0));
            push(Bit::Sep(theme::space::SM));
            push(Bit::Word(
                c"hardware conversion needs",
                theme::TEXT_SECONDARY,
                0,
            ));
            push(Bit::Air(theme::space::SM));
            push(Bit::Capsule);
        }
        PlayNote::Warn => {
            push(Bit::Glyph);
            push(Bit::Air(theme::space::SM));
            push(Bit::Word(c"HDR \u{2192} SDR", theme::TEXT_SECONDARY, 1));
            push(Bit::Sep(theme::space::SM));
            push(Bit::Word(c"tone-mapping needs", theme::TEXT_SECONDARY, 0));
            push(Bit::Air(theme::space::SM));
            push(Bit::Capsule);
        }
    }
    (bits, n)
}

fn bit_w(bit: Bit, measure: &dyn crate::ui::machine::Measure) -> f32 {
    match bit {
        Bit::Word(text, _, bold) => measure.width(text, theme::size::CAPTION, bold != 0),
        Bit::Sep(gap) => 2.0 * gap + measure.width(c"\u{b7}", theme::size::CAPTION, false),
        Bit::Glyph => FACTS_GLYPH_D,
        Bit::Air(gap) => gap,
        Bit::Capsule => crate::ui::widgets::pass_capsule_w(measure),
    }
}

fn play_mode_w(d: &Detail, after: bool, measure: &dyn crate::ui::machine::Measure) -> f32 {
    let (bits, n) = play_mode_bits(d, after);
    bits[..n].iter().copied().map(|b| bit_w(b, measure)).sum()
}

fn draw_play_mode(
    p: Painter,
    d: &Detail,
    x: f32,
    y: f32,
    after: bool,
    measure: &dyn crate::ui::machine::Measure,
) -> f32 {
    let (top, baseline) = crate::text::text_cap_band(theme::size::CAPTION, 0);
    let cy = y + (top + baseline) * 0.5;
    let (bits, n) = play_mode_bits(d, after);
    let mut bx = x;
    for bit in &bits[..n] {
        match *bit {
            Bit::Word(text, color, bold) => {
                bx += p.text(text.as_ptr(), bx, y, theme::size::CAPTION, color, 0, bold);
            }
            Bit::Sep(gap) => {
                p.text(
                    c"\u{b7}".as_ptr(),
                    bx + gap,
                    y,
                    theme::size::CAPTION,
                    theme::TEXT_SEPARATOR,
                    0,
                    0,
                );
                bx += bit_w(*bit, measure);
            }
            Bit::Glyph => {
                crate::ui::icons::draw(
                    p,
                    crate::ui::icons::Icon::Alert,
                    Rect::new(bx, cy - FACTS_GLYPH_D * 0.5, FACTS_GLYPH_D, FACTS_GLYPH_D),
                    theme::TEXT_SECONDARY,
                );
                bx += FACTS_GLYPH_D;
            }
            Bit::Air(gap) => bx += gap,
            Bit::Capsule => bx += crate::ui::widgets::pass_capsule(p, bx, cy, false, measure),
        }
    }
    bx - x
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FactsFit {
    extent: bool,
    credit: bool,
    elide: bool,
}

fn facts_fit(has_credit: bool, budget: f32, mut width: impl FnMut(FactsFit) -> f32) -> FactsFit {
    let mut fit = FactsFit {
        extent: true,
        credit: has_credit,
        elide: false,
    };
    if width(fit) <= budget {
        return fit;
    }
    if fit.credit {
        fit.credit = false;
        if width(fit) <= budget {
            return fit;
        }
    }
    fit.extent = false;
    fit.elide = width(fit) > budget;
    fit
}

fn facts_flow(
    parts: &[&str],
    credit: &str,
    mut run: impl FnMut(&str, f32, c_int, [f32; 4]) -> f32,
    mode: impl FnOnce(f32, bool) -> f32,
) -> f32 {
    fn separator(run: &mut impl FnMut(&str, f32, c_int, [f32; 4]) -> f32, dx: f32) -> f32 {
        2.0 * FACTS_SEP_PAD
            + run(
                "\u{b7}",
                dx + FACTS_SEP_PAD,
                theme::size::CAPTION,
                theme::TEXT_SEPARATOR,
            )
    }
    let mut dx = 0.0;
    let mut any = false;
    for part in parts.iter().filter(|part| !part.is_empty()) {
        if any {
            dx += separator(&mut run, dx);
        }
        dx += run(part, dx, theme::size::CAPTION, theme::TEXT_TERTIARY);
        any = true;
    }
    let mode_w = mode(dx, any);
    dx += mode_w;
    any |= mode_w > 0.0;
    if !credit.is_empty() {
        if any {
            dx += separator(&mut run, dx);
        }
        dx += run(credit, dx, theme::size::CAPTION, theme::TEXT_TERTIARY);
    }
    dx
}

pub(crate) fn draw_facts(p: Painter, d: &Detail, y: f32, measure: &dyn crate::ui::machine::Measure) {
    let (date, extent) = hero_facts(d);
    let extent = extent.as_deref().unwrap_or("");
    let credit = crate::ui::fmt::shared_by(&d.source()).unwrap_or_default();
    let width = |fit: FactsFit| {
        facts_flow(
            &[date.as_str(), if fit.extent { extent } else { "" }],
            if fit.credit { &credit } else { "" },
            |text, _, size, _| measure.width_str(text, size, false),
            |_, after| play_mode_w(d, after, measure),
        )
    };
    let fit = facts_fit(
        !credit.is_empty(),
        FACTS_R - crate::ui::consts::MARGIN_X,
        width,
    );
    let elided;
    let date = if fit.elide {
        let budget = (FACTS_R - crate::ui::consts::MARGIN_X - play_mode_w(d, true, measure)).max(0.0);
        elided = crate::text::elide_by(&date, budget, false, |t| {
            measure.width_str(t, theme::size::CAPTION, false)
        });
        elided.as_str()
    } else {
        date.as_str()
    };
    facts_flow(
        &[date, if fit.extent { extent } else { "" }],
        if fit.credit { &credit } else { "" },
        |text, dx, size, color| {
            CString::new(text)
                .ok()
                .map(|text| {
                    p.text(
                        text.as_ptr(),
                        crate::ui::consts::MARGIN_X + dx,
                        y,
                        size,
                        color,
                        0,
                        0,
                    )
                })
                .unwrap_or(0.0)
        },
        |dx, after| draw_play_mode(p, d, crate::ui::consts::MARGIN_X + dx, y, after, measure),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(restart: bool, alt: bool, mark: PosterMark) -> HeroSet {
        HeroSet { restart, alt, mark }
    }

    /// **The row offers exactly ONE watched toggle, never both faces at once** — the property
    /// `ui/detail.rs`'s own module doc calls out by name ("this row briefly drew the two faces
    /// side by side for a part-watched item, and that was wrong").
    #[test]
    fn the_row_offers_exactly_one_watched_toggle() {
        for mark in [
            PosterMark::None,
            PosterMark::InProgress,
            PosterMark::Watched,
        ] {
            for restart in [false, true] {
                for alt in [false, true] {
                    let s = set(restart, alt, mark);
                    let (v, n) = hero_ctls(s);
                    let watch_ctls: Vec<_> = v[..n].iter().filter(|c| c.is_watch()).collect();
                    assert_eq!(watch_ctls.len(), 1, "set={s:?}");
                }
            }
        }
    }

    /// Each of the row's discs writes its OWN verb: the restart disc always unfurls "Play from
    /// Start"; the watch toggle's verb depends on which face it wears AND on whether the hero is
    /// naming an episode's SHOW rather than the episode itself.
    #[test]
    fn each_watch_disc_writes_its_own_verb() {
        assert_eq!(
            disc_verb(HeroCtl::Restart, false),
            Some((0, PLAY_FROM_START_LABEL))
        );
        assert_eq!(
            disc_verb(HeroCtl::Restart, true),
            Some((0, PLAY_FROM_START_LABEL))
        );
        assert_eq!(
            disc_verb(HeroCtl::MarkWatched, false),
            Some((1, MARK_WATCHED_LABEL))
        );
        assert_eq!(
            disc_verb(HeroCtl::MarkWatched, true),
            Some((1, MARK_SHOW_WATCHED_LABEL))
        );
        assert_eq!(
            disc_verb(HeroCtl::MarkUnwatched, false),
            Some((1, MARK_UNWATCHED_LABEL))
        );
        assert_eq!(
            disc_verb(HeroCtl::MarkUnwatched, true),
            Some((1, MARK_SHOW_UNWATCHED_LABEL))
        );
        assert_eq!(
            disc_verb(HeroCtl::Play, false),
            None,
            "a pill has nothing to unfurl"
        );
        assert_eq!(disc_verb(HeroCtl::Alt, false), None);
    }

    /// The watched toggle is carried by its JOB, not by either face — `watch_index` must answer
    /// `Some` for a set wearing EITHER face, at the SAME index.
    #[test]
    fn the_watch_toggle_is_found_by_its_job_whichever_face_it_wears() {
        let unwatched = set(false, false, PosterMark::None);
        let watched = set(false, false, PosterMark::Watched);
        let i_unwatched = watch_index(unwatched).expect("always present");
        let i_watched = watch_index(watched).expect("always present");
        assert_eq!(
            i_unwatched, i_watched,
            "the toggle sits at the row's tail in both sets"
        );
        assert_eq!(ctl_at(unwatched, i_unwatched), Some(HeroCtl::MarkWatched));
        assert_eq!(ctl_at(watched, i_watched), Some(HeroCtl::MarkUnwatched));
    }

    /// A finished-then-restarted item reads `InProgress`, never `Watched` — the viewer is
    /// part-way through a re-watch, and "not watched" is the honest state for the toggle to read.
    #[test]
    fn a_restarted_finished_item_reads_in_progress_not_watched() {
        // `watched=true` but a live resume point beats it.
        assert_eq!(
            hero_watch_state(false, false, true, 4_000_000_000),
            PosterMark::InProgress
        );
    }

    /// A show mid-run (started, not finished) reads `InProgress` even with no resume point of its
    /// own — `mid_run` is `is_show && show_started && !watched`.
    #[test]
    fn a_show_mid_run_reads_in_progress_with_no_resume_point_of_its_own() {
        assert_eq!(
            hero_watch_state(true, true, false, 0),
            PosterMark::InProgress
        );
        assert_eq!(
            hero_watch_state(true, true, true, 0),
            PosterMark::Watched,
            "…but a FINISHED show is simply watched"
        );
        assert_eq!(
            hero_watch_state(true, false, false, 0),
            PosterMark::None,
            "an unstarted show is simply unstarted"
        );
    }

    /// **`hero_col`'s reconciliation, restated as identity-membership**: a restart control that
    /// vanishes from the set (a resume point was just consumed) is no longer found by
    /// `index_of`, which is exactly the signal `DetailScreen::reconcile` uses to know the focus
    /// must move off it.
    #[test]
    fn restart_disc_tracks_the_resume_the_play_would_actually_apply() {
        assert!(!has_restart(0), "no resume point, no restart disc");
        assert!(has_restart(1), "any positive resume position offers one");
        let with = set(true, false, PosterMark::None);
        let without = set(false, false, PosterMark::None);
        assert_eq!(index_of(with, HeroCtl::Restart), Some(1));
        assert_eq!(
            index_of(without, HeroCtl::Restart),
            None,
            "the disc that a completed restart just consumed is gone from the new set"
        );
    }

    /// The row's ACCUMULATED x positions never overlap and advance strictly left to right, at
    /// every set size the row can take (the property the pointer hit-test and the draw both rely
    /// on agreeing about).
    #[test]
    fn the_row_s_controls_never_overlap_at_any_set_size() {
        let cw = HeroWidths {
            pill: 200.0,
            alt: 260.0,
            disc: [CD, CD],
        };
        for restart in [false, true] {
            for alt in [false, true] {
                for mark in [PosterMark::None, PosterMark::Watched] {
                    let s = set(restart, alt, mark);
                    let (_, n) = hero_ctls(s);
                    let mut prev_right = f32::MIN;
                    for i in 0..n {
                        let r = hero_btn_rect_at(s, i, 0.0, cw);
                        assert!(r.x >= prev_right, "set={s:?} i={i} rect={r:?}");
                        prev_right = r.x + r.w;
                    }
                }
            }
        }
    }

    /// `ctl_at`/`index_of` are exact inverses over every index the set actually has.
    #[test]
    fn ctl_at_and_index_of_round_trip() {
        for restart in [false, true] {
            for alt in [false, true] {
                for mark in [
                    PosterMark::None,
                    PosterMark::InProgress,
                    PosterMark::Watched,
                ] {
                    let s = set(restart, alt, mark);
                    let (_, n) = hero_ctls(s);
                    for i in 0..n {
                        let ctl = ctl_at(s, i).unwrap();
                        assert_eq!(index_of(s, ctl), Some(i));
                    }
                    assert_eq!(
                        ctl_at(s, n),
                        None,
                        "one past the end is None, never a panic"
                    );
                }
            }
        }
    }

    #[test]
    fn the_two_spellings_of_the_conversion_notice_are_the_same_bytes() {
        assert_eq!(
            CONVERTS_ON_SERVER_C.to_str().unwrap(),
            crate::ui::fmt::CONVERTS_ON_SERVER
        );
    }

    #[test]
    fn how_it_plays_resolves_the_full_docs_truth_table() {
        use crate::plex::serverinfo::Subscription::{No, Unknown, Yes};
        use crate::route::Preview::{Converts, DirectPlay, Remux};
        for preview in [DirectPlay, Remux, Converts] {
            for hdr in [false, true] {
                for subscription in [Unknown, No, Yes] {
                    let expected = match (preview, hdr, subscription) {
                        (Converts, true, No) => PlayNote::Warn,
                        (Converts, false, No) => PlayNote::Soft,
                        _ => PlayNote::Quiet,
                    };
                    assert_eq!(play_note(preview, hdr, subscription), expected);
                }
            }
        }
    }

    #[test]
    fn the_pass_note_judges_the_items_own_server_not_the_browsed_one() {
        let _guard = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let own = crate::plex::register_for_test("own", "127.0.0.1", 1, "t", "c1");
        let shared = crate::plex::register_for_test("shared", "127.0.0.2", 2, "t", "c2");
        crate::plex::serverinfo::store_for_test(
            own,
            crate::plex::serverinfo::Subscription::Yes,
            "1",
        );
        crate::plex::serverinfo::store_for_test(
            shared,
            crate::plex::serverinfo::Subscription::No,
            "1",
        );
        let borrowed = Detail {
            sid: shared,
            ..Default::default()
        };
        let ours = Detail {
            sid: own,
            ..Default::default()
        };
        assert_eq!(
            item_subscription(&borrowed),
            crate::plex::serverinfo::Subscription::No
        );
        assert_eq!(
            item_subscription(&ours),
            crate::plex::serverinfo::Subscription::Yes
        );
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn an_item_on_your_own_server_gets_no_source_run_at_all() {
        #[derive(Debug, PartialEq)]
        enum Event {
            Run(String),
            Mode,
        }
        let events = std::cell::RefCell::new(Vec::new());
        facts_flow(
            &["13 Jun 2024", "1 hr 57 min"],
            "",
            |text, _, _, _| {
                events.borrow_mut().push(Event::Run(text.into()));
                text.len() as f32
            },
            |_, _| {
                events.borrow_mut().push(Event::Mode);
                5.0
            },
        );
        assert_eq!(events.into_inner().last(), Some(&Event::Mode));
    }

    #[test]
    fn the_source_credit_is_the_last_run_on_the_line_after_the_play_mode_fragment() {
        let events = std::cell::RefCell::new(Vec::new());
        facts_flow(
            &["date", "extent"],
            "Shared by friend",
            |text, _, _, _| {
                events.borrow_mut().push(text.to_string());
                text.len() as f32
            },
            |_, _| {
                events.borrow_mut().push("<mode>".into());
                5.0
            },
        );
        assert_eq!(
            events.into_inner().last().map(String::as_str),
            Some("Shared by friend")
        );
    }

    #[test]
    fn a_credit_with_nothing_in_front_of_it_opens_the_line_bare() {
        let placed = std::cell::RefCell::new(Vec::new());
        facts_flow(
            &["", ""],
            "Shared by friend",
            |text, x, _, _| {
                placed.borrow_mut().push((text.to_string(), x));
                text.len() as f32
            },
            |_, _| 0.0,
        );
        assert_eq!(placed.into_inner(), vec![("Shared by friend".into(), 0.0)]);
    }

    #[test]
    fn the_trailing_credit_is_the_first_member_the_row_gives_up() {
        let width = |fit: FactsFit| {
            100.0 + if fit.extent { 100.0 } else { 0.0 } + if fit.credit { 150.0 } else { 0.0 }
        };
        assert_eq!(
            facts_fit(true, 349.0, width),
            FactsFit {
                extent: true,
                credit: false,
                elide: false
            }
        );
        assert_eq!(
            facts_fit(true, 199.0, width),
            FactsFit {
                extent: false,
                credit: false,
                elide: false
            }
        );
        assert!(facts_fit(true, 99.0, width).elide);
    }

    #[test]
    fn the_crew_credit_names_the_right_job_for_the_kind_of_item() {
        fn credit(name: &str, role: &str) -> crate::metadata::Cast {
            crate::metadata::Cast {
                tag: name.into(),
                role: role.into(),
                thumb: String::new(),
                id: 0,
                tag_key: String::new(),
            }
        }
        let movie = Detail {
            directors: vec!["Jane Doe".into()],
            ..Default::default()
        };
        assert_eq!(hero_credit(&movie), Some(("Directed by", vec!["Jane Doe"])));
        let show = Detail {
            is_show: true,
            crew: vec![
                credit("Pilot Director", "Director"),
                credit("Kerry Ehrin", "Writer"),
            ],
            ..Default::default()
        };
        assert_eq!(
            hero_credit(&show),
            Some(("Created by", vec!["Kerry Ehrin"]))
        );
    }

    #[test]
    fn restart_and_the_watch_tail_are_independent() {
        let set = HeroSet {
            restart: true,
            alt: false,
            mark: PosterMark::Watched,
        };
        let (controls, n) = hero_ctls(set);
        assert_eq!(
            &controls[..n],
            &[HeroCtl::Play, HeroCtl::Restart, HeroCtl::MarkUnwatched]
        );
    }

    #[test]
    fn the_actions_row_grows_an_also_available_control_only_for_a_second_source() {
        let without = HeroSet {
            restart: false,
            alt: false,
            mark: PosterMark::None,
        };
        let with = HeroSet {
            alt: true,
            ..without
        };
        assert!(index_of(without, HeroCtl::Alt).is_none());
        assert!(index_of(with, HeroCtl::Alt).is_some());
    }

    #[test]
    fn the_hero_row_carries_no_track_information_disc() {
        for alt in [false, true] {
            let set = HeroSet {
                restart: true,
                alt,
                mark: PosterMark::InProgress,
            };
            let (controls, n) = hero_ctls(set);
            assert!(controls[..n].iter().all(|ctl| matches!(
                ctl,
                HeroCtl::Play
                    | HeroCtl::Restart
                    | HeroCtl::Alt
                    | HeroCtl::MarkWatched
                    | HeroCtl::MarkUnwatched
            )));
        }
    }

    #[test]
    fn a_shows_hero_is_about_the_servers_on_deck_episode_or_the_series() {
        let mut d = Detail {
            is_show: true,
            seasons: vec![crate::metadata::Season {
                rk: "s1".into(),
                index: 1,
                title: "Season 1".into(),
                leaf_count: 3,
                viewed_leaf_count: 0,
            }],
            on_deck: Some(Episode {
                rk: "e1".into(),
                resume_ms: 0,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            hero_episode(&d).is_none(),
            "an untouched show is about the series"
        );
        d.seasons[0].viewed_leaf_count = 1;
        assert_eq!(hero_episode(&d).map(|ep| ep.rk.as_str()), Some("e1"));
    }

    #[test]
    fn the_watch_state_resolver_answers_leaf_and_container_by_their_own_rules() {
        assert_eq!(hero_watch_state(false, false, false, 0), PosterMark::None);
        assert_eq!(hero_watch_state(false, false, true, 0), PosterMark::Watched);
        assert_eq!(
            hero_watch_state(true, true, false, 0),
            PosterMark::InProgress
        );
        assert_eq!(hero_watch_state(true, true, true, 0), PosterMark::Watched);
    }

    #[test]
    fn the_optimistic_flip_settles_a_leaf_at_once_and_a_container_a_round_trip_late() {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        crate::metadata::set_current_for_test(Some(Detail {
            sid,
            rk: "movie".into(),
            resume_ms: 1_800_000,
            dur_ms: 7_200_000,
            ..Default::default()
        }));
        assert!(crate::stores::metadata::apply(
            crate::stores::metadata::MetadataCmd::SetWatchedLocal { sid, rk: "movie".into(), on: true }
        ));
        let movie = crate::metadata::current().unwrap();
        assert_eq!(hero_mark(movie), PosterMark::Watched);
        assert_eq!(
            movie.resume_ms, 0,
            "a leaf's restart disc disappears immediately"
        );

        crate::metadata::set_current_for_test(Some(Detail {
            sid,
            rk: "show".into(),
            is_show: true,
            seasons: vec![crate::metadata::Season {
                rk: "s1".into(),
                index: 1,
                title: "Season 1".into(),
                leaf_count: 2,
                viewed_leaf_count: 1,
            }],
            on_deck: Some(Episode {
                resume_ms: 600_000,
                dur_ms: 2_700_000,
                ..Default::default()
            }),
            ..Default::default()
        }));
        assert!(crate::stores::metadata::apply(
            crate::stores::metadata::MetadataCmd::SetWatchedLocal { sid, rk: "show".into(), on: true }
        ));
        assert_eq!(
            hero_mark(crate::metadata::current().unwrap()),
            PosterMark::InProgress,
            "container progress remains server evidence until the re-read lands"
        );
        crate::metadata::set_current_for_test(Some(Detail {
            sid,
            rk: "show".into(),
            is_show: true,
            watched: true,
            ..Default::default()
        }));
        assert_eq!(
            hero_mark(crate::metadata::current().unwrap()),
            PosterMark::Watched
        );
        crate::metadata::set_current_for_test(None);
    }

    #[test]
    fn hero_indices_mean_different_actions_in_the_two_control_sets() {
        let compact = HeroSet {
            restart: false,
            alt: false,
            mark: PosterMark::None,
        };
        let wide = HeroSet {
            restart: true,
            alt: true,
            mark: PosterMark::None,
        };
        assert_eq!(ctl_at(compact, 1), Some(HeroCtl::MarkWatched));
        assert_eq!(ctl_at(wide, 1), Some(HeroCtl::Restart));
        assert_eq!(ctl_at(wide, 2), Some(HeroCtl::Alt));
    }

    #[test]
    fn the_actions_row_accumulates_around_a_variable_width_control() {
        let set = HeroSet {
            restart: true,
            alt: true,
            mark: PosterMark::None,
        };
        let widths = HeroWidths {
            pill: 210.0,
            alt: 300.0,
            disc: [90.0, 120.0],
        };
        let (controls, n) = hero_ctls(set);
        for i in 1..n {
            let previous = hero_btn_rect_at(set, i - 1, 0.0, widths);
            let current = hero_btn_rect_at(set, i, 0.0, widths);
            assert_eq!(
                current.x,
                previous.x + previous.w + CGAP,
                "{:?}",
                controls[i]
            );
        }
    }

    #[test]
    fn an_unfurling_disc_never_crosses_the_people_column() {
        let mut opened = false;
        for restart in [false, true] {
            for alt in [false, true] {
                for mark in [
                    PosterMark::None,
                    PosterMark::InProgress,
                    PosterMark::Watched,
                ] {
                    let set = HeroSet { restart, alt, mark };
                    let (_, n) = hero_ctls(set);
                    for labels in [[10.0, 12.0], [201.0, 316.0], [400.0, 400.0], [900.0, 40.0]] {
                        for e in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                            for unfurl in [[e, 1.0 - e], [1.0 - e, e], [e, 0.0], [0.0, e]] {
                                let disc = disc_caps_at(set, PW + 62.0, 340.0, unfurl, labels);
                                let last = hero_btn_rect_at(
                                    set,
                                    n - 1,
                                    0.0,
                                    HeroWidths {
                                        pill: PW + 62.0,
                                        alt: 340.0,
                                        disc,
                                    },
                                );
                                assert!(last.x + last.w <= FACTS_R + 0.01);
                                opened |= disc.iter().any(|width| *width > CD);
                            }
                        }
                    }
                }
            }
        }
        assert!(opened, "the sweep must exercise a real open capsule");
    }

    #[test]
    fn the_real_verbs_all_fit_the_widest_row() {
        let measure = crate::ui::fixture::FixtureMeasure;
        let set = HeroSet {
            restart: true,
            alt: true,
            mark: PosterMark::Watched,
        };
        let caps = disc_caps(&measure, set, [1.0, 1.0], true);
        assert!(caps.iter().all(|width| *width >= CD));
        assert!(caps.iter().any(|width| *width > CD));
    }

    struct HugeMeasure;
    impl Measure for HugeMeasure {
        fn width(&self, _text: &CStr, _size: i32, _bold: bool) -> f32 {
            10_000.0
        }
        fn cap_h(&self, size: i32) -> f32 {
            size as f32
        }
        fn line_h(&self, size: i32) -> f32 {
            size as f32
        }
    }

    #[test]
    fn a_verb_that_does_not_fit_is_dropped_whole() {
        let set = HeroSet {
            restart: true,
            alt: true,
            mark: PosterMark::Watched,
        };
        assert_eq!(disc_caps(&HugeMeasure, set, [1.0, 1.0], true), [CD; 2]);
    }

    #[test]
    fn the_unfurled_verbs_are_the_menus_own() {
        assert_eq!(
            disc_verb(HeroCtl::Restart, false)
                .unwrap()
                .1
                .to_str()
                .unwrap(),
            "Play from Start"
        );
        assert_eq!(
            disc_verb(HeroCtl::MarkWatched, false)
                .unwrap()
                .1
                .to_str()
                .unwrap(),
            "Mark as Watched"
        );
        assert_eq!(
            disc_verb(HeroCtl::MarkUnwatched, true)
                .unwrap()
                .1
                .to_str()
                .unwrap(),
            "Mark Show as Unwatched"
        );
    }

    #[test]
    fn the_widest_action_row_clears_the_people_column() {
        let set = HeroSet {
            restart: true,
            alt: true,
            mark: PosterMark::InProgress,
        };
        let (_, n) = hero_ctls(set);
        let last = hero_btn_rect_at(
            set,
            n - 1,
            0.0,
            HeroWidths {
                pill: PW + 62.0,
                alt: 340.0,
                disc: [CD; 2],
            },
        );
        assert!(last.x + last.w < FACTS_R);
    }
}
