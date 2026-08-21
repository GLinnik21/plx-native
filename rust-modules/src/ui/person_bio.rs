//! The person page's **bio alert panel** — the full biography, behind the truncation mark.
//!
//! **The reference is `Alert Views.dc.html` §1C · Person** (the owner's design project): a centred
//! 1120×700 glass sheet with 48px of padding and a [`theme::ALERT_PANEL_RAD`] corner, holding an
//! eyebrow, the name, an identity line, and — between two hairlines — the biography as a
//! **scrolling, paged viewport** with a feathered edge and a rail down its right side. The footer
//! states what the page knows about this person's presence in the library, and how to leave.
//!
//! **It is the panel BEHIND the mark, not a replacement for it.** `person.rs`'s bio block dissolves
//! its third line into a right-pinned `MORE` (`TextView::fade_last` + the pinned label), and that
//! affordance is unchanged: `MORE` is still a mark rather than a control, still unfocusable, still
//! drawn only when `bio.truncates(BIO_W)`. This panel is what OK on the header now opens, and it is
//! gated on **exactly that same predicate** — [`person::bio_is_truncated`]. A panel that opened on a
//! two-line bio would show the reader the words they had just finished reading.
//!
//! ## Three things here are not obvious
//!
//! **1. The scrim belongs to the PAGE, and it is heavier than a menu's.** [`scrim`] is called from
//! `person::draw` before the panel, not from [`draw`] — the rule `Popover::scrim` states for a
//! refreshing backdrop, kept here because it is also what makes the *contrast* argument work. The
//! design's host frame carries a standing comment that nothing bright may sit inside the panel's own
//! rectangle: a 72px name read through a 72% frost lifts the ground under the fine print past its
//! graded contrast. On the real page that name is not hypothetical — `person.rs` draws the person's
//! own name at `size::HERO` at x≈474, y≈96, which is *directly behind this panel's top-left corner*,
//! where the eyebrow and the identity line sit. The mock could stage that away; we cannot, so the
//! page is dimmed at [`SCRIM_A`] — [`theme::SCRIM_TEXT_A`], the measured text-legibility floor —
//! before the frost ever samples it. The anchored chip menus use 0.45 because nothing of theirs is
//! fine print over a headline; this panel's whole lower half is.
//!
//! **2. Paragraphs are separate views, because `TextView` cannot hold them.** `TextView::wrap`
//! splits on `char::is_whitespace` and reflows the lot, so a `\n\n` in a plex.tv biography is
//! destroyed — every paragraph break in the source becomes a single space. The block is therefore
//! [`paragraphs`] + one memoised `TextView` each, stacked on [`PARA_GAP`]. That also buys the
//! per-paragraph cull for free.
//!
//! **3. Nothing here animates from a clock.** The scroll is a [`Spring`], so `gfx::spring` reports
//! it to [`crate::ui::idle`] and the present gate sees the paging motion without this module opting
//! in. That is deliberate rather than incidental — `Xfade` and `Spinner` both shipped FROZEN behind
//! that gate because they integrate milliseconds, and a hand-rolled scroll offset here would have
//! been the third. The discrete transitions ([`open`]/[`close`]/[`move_focus`]) still call
//! `idle::invalidate` because a state change is not motion.
use crate::person::Person;
use crate::ui::consts::{SCR_H, SCR_W, SDLK_DOWN, SDLK_UP};
use crate::ui::label::{Label, VAlign};
use crate::ui::popover::Popover;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets;
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_uint;
use std::ptr::{addr_of, addr_of_mut};

// ---- geometry (the design's numbers, and everything else derived from them) --------------------

/// The sheet, centred. 1120×700 is the design's alert class — wide enough for a comfortable measure
/// of body text at [`theme::size::BODY`] and short enough to leave the page legible around it.
const PANEL_W: f32 = 1120.0;
const PANEL_H: f32 = 700.0;
/// The one padding, on all four sides. Quoted with [`theme::ALERT_PANEL_RAD`] because the two constrain
/// each other — see that token for why a bigger corner would eat this box.
const PAD: f32 = 48.0;

/// How far the sheet rises into place, matching every other popover in the app.
const RISE: f32 = Popover::RISE;
/// The page dim. See the module doc — this is heavier than the chip menus' 0.45 on purpose, and it
/// is a named token rather than a tuned number.
const SCRIM_A: f32 = theme::SCRIM_TEXT_A;

/// Air between paragraphs of the biography. A block gap, one `space` rung — the paragraphs are
/// separate thoughts, not separate blocks of the page.
const PARA_GAP: f32 = theme::space::MD;
/// The design's `trailing 40px spacer` under the last paragraph (§1C; §1B spends the same as its
/// `padding-bottom:40`). It is air at the END OF THE TRAVEL, not padding: without it the last line
/// rests flush on the viewport's clip, and the bottom feather is off by then — the block has
/// stopped scrolling — so the final line of a biography was cut by a hard edge with nothing under
/// it. That is the one page of this panel a reader always reaches.
const BODY_TAIL: f32 = theme::space::LG;
/// The bio's line pitch. `person.rs`'s header bio uses the same 40 at the same rung, so the prose
/// is paced identically whether it is being previewed or read.
const BIO_LEAD: f32 = 40.0;

/// One press of UP/DOWN. The design's number, and deliberately NOT a full viewport: a page that
/// replaces every visible line gives the eye nothing to re-anchor on, so a step leaves several
/// lines of overlap.
const STEP: f32 = 200.0;
/// The feather band at each end of the viewport — see [`widgets::edge_feather`].
const FEATHER: f32 = 88.0;
/// Air between the prose column and the rail beside it.
const RAIL_GAP: f32 = theme::space::MD;

/// Letter-tracking on the eyebrow: `.08em` of its own rung. A kicker is read as a LABEL rather than
/// as a word, and tracking is what says so; the [`widgets::pass_capsule`] label spends .06em for the
/// same reason one rung of emphasis down.
const EYEBROW_TRACK: f32 = theme::size::CAPTION as f32 * 0.08;
/// The eyebrow, pre-split per character — [`widgets::tracked_run`] explains why a tracked run is
/// spelled this way, and why it is only worth it for a constant word.
const EYEBROW: [&std::ffi::CStr; 6] = [c"P", c"E", c"R", c"S", c"O", c"N"];

/// The footer's right-hand hint, as its three runs — assembled by the shared
/// [`widgets::KeyHint`], which owns the cap, the gaps and the measure. This module used to lay the
/// line out itself (three `text_width` calls, its own `HINT_GAP` and a bare `key_cap`), which was
/// the same arithmetic the widget already does and the place a fourth copy of the design's `gap`
/// would have drifted — it did drift, to 14 against the spec's 12.
const HINT_PRE: &std::ffi::CStr = c"Press";
const HINT_KEY: &std::ffi::CStr = c"BACK";
const HINT_POST: &std::ffi::CStr = c"to return";

/// The dot-separated identity line's air either side of its separator — `detail.rs`'s facts row
/// spends the same, and the two lines are the same idiom.
const META_SEP_PAD: f32 = 10.0;

// ---- state -------------------------------------------------------------------------------------

/// **A DYNAMIC backdrop, not the default cached one, and the reason is measurable.**
///
/// `Glass::CACHED` takes its one snapshot at the panel's FIRST draw — the frame [`open`] was pressed
/// on, when the appear spring is still ~0 and [`scrim`] is therefore drawing at ~0 alpha too. The
/// frost then spends the whole session sampling an **undimmed** page, which on this page is the
/// exact failure the design's contrast note describes. Measured in the simulator before this line
/// went in: the panel's ground ran a mean luminance of **56.9 under the identity line** against
/// **34.0 under the prose** four inches below it — a 23-level lift, and it is the blurred ghost of
/// the page's own `size::HERO` name sitting behind the panel's fine print.
///
/// A refreshing policy re-sources the backdrop on its own cadence, so once the scrim has ramped in
/// the frost is sampling the page the user can actually see. It is also what makes the
/// scrim-belongs-to-the-page rule mandatory rather than stylistic here: `Glass::needs_page_scrim`
/// is true for this policy, and `Popover::painter` `debug_assert`s against a panel that tries to
/// draw its own dim.
static mut POP: Popover = Popover::with_glass(crate::ui::widgets::Glass::DYNAMIC_BACKDROP);
/// The current page, 1-based. The scroll spring chases [`scroll_for_page`] of it, rather than the
/// page being derived from the scroll: paging is the input, and a spring that is still travelling
/// must not be read back as a different page half way there.
static mut PAGE: usize = 1;
static mut SCROLL: Spring = Spring::at(0.0);

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// The focused page, for the focus probe (`crate::focusprobe`) — this panel's UP/DOWN moves this
/// and nothing else, so the fingerprint is blind to it without a reader. Same reason
/// `chapters_panel::sel` and `info_panel::sel` exist.
pub(crate) fn page() -> usize {
    unsafe { addr_of!(PAGE).read() }
}

/// Open the panel at the top of the biography.
///
/// The caller owns the GATE, not this function — `person::on_ok` opens it only when the bio is
/// actually truncated. Kept that way round because the predicate is the *page's* (it depends on
/// `person::BIO_W`, the header's column width), and duplicating it here is how the mark and the
/// panel would come to disagree about whether there is more to read.
pub(crate) fn open() {
    unsafe {
        addr_of_mut!(PAGE).write(1);
        addr_of_mut!(SCROLL).write(Spring::at(0.0));
    }
    pop().open();
    crate::ui::idle::invalidate();
}

pub(crate) fn close() {
    if is_open() {
        crate::ui::idle::invalidate();
    }
    pop().close();
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    // Re-clamp before springing: the store can land a longer (or empty) biography while the panel
    // is up — `person::pump` applies a profile whenever it arrives — and a page index past the end
    // would otherwise park the spring beyond the content.
    let (_, pages) = page_state();
    let pg = page().clamp(1, pages);
    unsafe { addr_of_mut!(PAGE).write(pg) };
    let want = scroll_for_page(pg);
    let sc = unsafe { &mut *addr_of_mut!(SCROLL) };
    sc.step(want, crate::ui::consts::K_SCROLL, dt);
    crate::ui::anim::probe("personbio.scroll", sc.pos, sc.vel, want, dt);
}

/// UP/DOWN page the viewport; LEFT/RIGHT are inert (there is one column of prose, and nothing
/// beside it to move to). Focus is TRAPPED here while the panel is up — `person::move_focus`
/// forwards to this and returns, so the page behind cannot walk its shelves under the sheet.
///
/// **It does not clamp against the page COUNT, and that is load-bearing rather than lazy.**
/// [`update`] already re-clamps every frame — it has to, since the store can land a longer (or
/// empty) biography while the panel is open — so a second clamp here would be a duplicate. It would
/// also be an expensive one: knowing `pages` means measuring the wrapped prose, which reaches
/// `TextView` → `crate::text` → `TTF_SizeUTF8`.
///
/// **Doing that from a key handler does not fail as a skipped test. It fails as a LINK ERROR.**
/// `person::move_focus` is called by the host suite, `cargo test --lib` builds without
/// `--features hostsim`, and nothing then supplies SDL_ttf or GL — so one `.min(pages)` on this
/// line stopped the whole 880-test suite from BUILDING, with an undefined `_TTF_SizeUTF8` naming
/// `crate::text` and nothing about this panel. It cost a bisect to find. Anything reachable from a
/// key handler on this page has to stay clear of text measurement.
///
/// So the index may run one past the end for a single frame and is pulled back before anything
/// reads it: `update` clamps, then computes the scroll target, and `draw` reads [`page`] after
/// both. Nothing on screen can observe the overshoot.
pub(crate) fn move_focus(sym: c_uint) {
    let pg = page();
    let next = match sym {
        SDLK_UP => pg.saturating_sub(1).max(1),
        SDLK_DOWN => pg + 1,
        _ => pg,
    };
    if next != pg {
        unsafe { addr_of_mut!(PAGE).write(next) };
        crate::ui::idle::invalidate();
    }
}

// ---- the measured flow (pure where it can be) ---------------------------------------------------

/// The biography split into paragraphs.
///
/// Blank-line delimited, which is what plex.tv's `summary` uses, and every run trimmed. Empty runs
/// are dropped rather than kept as zero-height rows, so a summary that ends in three newlines does
/// not leave a page of air under the last line.
///
/// Pure, and the reason this module can hold paragraphs at all — see the module doc for why
/// `TextView` cannot.
pub(crate) fn paragraphs(bio: &str) -> Vec<&str> {
    bio.split("\n\n")
        .flat_map(|b| b.split("\r\n\r\n"))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The panel's rect, and the content box inside its padding.
fn panel_rect() -> Rect {
    Rect::new((SCR_W - PANEL_W) * 0.5, (SCR_H - PANEL_H) * 0.5, PANEL_W, PANEL_H)
}
fn content_rect() -> Rect {
    panel_rect().inset(PAD)
}

/// The prose column's wrap width — the content box less the rail and its air. The rail is part of
/// the reading block's frame, so the text may not flow under it.
fn text_w() -> f32 {
    (content_rect().w - widgets::RAIL_W - RAIL_GAP).max(1.0)
}

/// One paragraph's view. Built in ONE place so its measure and its draw cannot disagree about the
/// rung, the ink or the leading — `person.rs::bio_view` carries the same note for the same reason.
fn para_view(text: &str) -> TextView<'_> {
    TextView::new(text, theme::size::BODY, theme::TEXT_READING).leading(BIO_LEAD)
}

/// The scrolling viewport's rect — what the clip cuts to and what the feather rides.
///
/// Everything above and below it is measured from the CONTENT box's two ends and the viewport takes
/// what is left, so the panel's fixed 700px height is spent on the prose rather than on air: a
/// header line that grows (a person with no roles, a very long name) shortens the reading window
/// instead of pushing the footer off the sheet.
fn viewport() -> Rect {
    let c = content_rect();
    let top = c.y + head_h() + theme::space::MD + 1.0 + theme::space::MD;
    let bottom = c.y + c.h - foot_h() - theme::space::MD - 1.0 - theme::space::MD;
    Rect::new(c.x, top, text_w(), (bottom - top).max(0.0))
}

/// The header block's height: eyebrow, name, identity line, on their `space` rungs. Measured from
/// the cap bands rather than the point sizes — `ui::label`'s layout-≠-paint rule.
fn head_h() -> f32 {
    crate::text::cap_h(theme::size::CAPTION, 1)
        + theme::space::SM
        + crate::text::cap_h(theme::size::TITLE, 1)
        + theme::space::SM
        + crate::text::cap_h(theme::size::CAPTION, 0)
}

/// The footer band's height — the keycap is the tallest thing in it, so the band is the cap.
fn foot_h() -> f32 {
    widgets::KeyHint::height()
}

/// Total flowed height of the biography at the current wrap width, and the viewport's height.
fn content_h(person: &Person) -> f32 {
    let w = text_w();
    let paras = paragraphs(&person.bio);
    if paras.is_empty() {
        return 0.0;
    }
    let mut h = 0.0;
    for (i, para) in paras.iter().enumerate() {
        if i > 0 {
            h += PARA_GAP;
        }
        h += para_view(para).measure_h(w);
    }
    h + BODY_TAIL
}

/// **The paging arithmetic, pure**: how many pages `content_h` of prose makes in a `view_h`
/// viewport stepping `step` px, and the furthest the block may scroll.
///
/// A page is a scroll POSITION, not a screenful — the step (200px) is smaller than the viewport, so
/// consecutive pages overlap. `pages` is therefore "how many distinct resting places are there",
/// which is one more than the number of whole steps the travel contains, and the LAST page is
/// pinned to `max_scroll` rather than to `(pages-1) * step`: the final step is a short one, and a
/// rail that stopped short of its track's end while the prose had visibly run out would be lying.
///
/// Content that fits is one page with no travel, which is what makes the rail and both feather
/// edges disappear together for a short biography.
pub(crate) fn paging(content_h: f32, view_h: f32, step: f32) -> (f32, usize) {
    let max_scroll = (content_h - view_h).max(0.0);
    if max_scroll <= 0.0 || step <= 0.0 {
        return (0.0, 1);
    }
    (max_scroll, (max_scroll / step).ceil() as usize + 1)
}

/// The scroll offset page `page` rests at — `(page-1)` whole steps, clamped to the travel. Pure.
pub(crate) fn scroll_at(page: usize, max_scroll: f32, step: f32) -> f32 {
    ((page.max(1) - 1) as f32 * step).min(max_scroll).max(0.0)
}

/// `(max_scroll, pages)` for the open person — the impure wrapper the screen calls.
fn page_state() -> (f32, usize) {
    let Some(person) = crate::person::current() else { return (0.0, 1) };
    paging(content_h(person), viewport().h, STEP)
}

fn scroll_for_page(page: usize) -> f32 {
    let (max_scroll, _) = page_state();
    scroll_at(page, max_scroll, STEP)
}

// ---- the two lines of prose the panel writes itself ---------------------------------------------

/// The identity line's runs, in flow order — **`roles · born · died · birthplace`**, with every
/// absent fact absent rather than blank.
///
/// Pure, and the shape is the point: it builds a LIST for [`widgets::dotted_run`] rather than
/// filling one template with holes, so a person plex.tv knows nothing about produces an empty line
/// the flow simply does not draw, and someone with a death date but no birth date reads
/// "Died 2 June 2017" with no leading separator. That is `person::refresh_runs`' rule for the
/// header's life line, applied to a line that also carries the roles.
///
/// The dates arrive pre-formatted; this function does no date work, so it stays testable without a
/// locale or a clock.
pub(crate) fn meta_runs(roles: &str, born: &str, died: &str, birthplace: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    // `Born ` is the design's own wording (§1C: "Actor, Singer" · "Born 8 Jan 1987" · "London,
    // England"), and it is not decoration: a bare date sitting between a role list and a city has
    // nothing to say which date it is. Its sibling `Died ` was labelled from the start, which is
    // what made the asymmetry easy to miss.
    for (label, value) in [("", roles), ("Born ", born), ("Died ", died), ("", birthplace)] {
        let value = value.trim();
        if !value.is_empty() {
            v.push(format!("{label}{value}"));
        }
    }
    v
}

/// The footer's left-hand line: **what this page actually knows** about the person's presence in
/// the library, which is the two shelf totals it is already showing.
///
/// Pluralised honestly, and the two kinds are named rather than folded into a neutral "title":
/// "1 film and 3 shows" is a fact the page can defend, "4 titles" is a number that hides which
/// shelves it came from. `None` when there is nothing — the panel then draws no line at all, rather
/// than a "0 films" that reads as a failed fetch. The person page's own empty state already says so
/// in words, underneath.
///
/// The counts are the RESPONSE totals (`Person::total`), not the drawn tile counts — the shelves cap
/// at a `CardRow`'s spring count, and "24" on a person with 60 films would be the cap posing as a
/// fact about the library.
pub(crate) fn library_line(films: usize, shows: usize) -> Option<String> {
    let noun = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let body = match (films, shows) {
        (0, 0) => return None,
        (f, 0) => noun(f, "film", "films"),
        (0, s) => noun(s, "show", "shows"),
        (f, s) => format!("{} and {}", noun(f, "film", "films"), noun(s, "show", "shows")),
    };
    Some(format!("{body} in this library"))
}

// ---- draw ----------------------------------------------------------------------------------------

/// The page's half of the modal: the full-screen dim, drawn by `person::draw` BEFORE the panel.
///
/// See the module doc for both halves of why it lives here rather than inside [`draw`] — the
/// `Popover::scrim` contract, and the contrast argument that makes a heavy scrim load-bearing on
/// this particular page.
pub(crate) fn scrim() {
    if is_open() {
        pop().scrim(SCRIM_A);
    }
}

/// Resolve the glass cadence before the host page draws — every popover's route arm calls this
/// unconditionally; a closed one prepares nothing.
pub(crate) fn prepare_present(underlay_changed: bool) {
    pop().prepare_present(underlay_changed);
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    // **A surface may not appear in its own backdrop.** The direct blur-source path re-renders the
    // host page into a small target, and this panel is drawn from inside `person::draw` — so
    // without this the frost would be sampling a quarter-scale copy of itself, one refresh stale.
    // The SCRIM deliberately does not take this branch: it is page content, and its whole job is to
    // be in what the frost samples.
    if crate::gfx::blur_source_pass() {
        return;
    }
    let Some(person) = crate::person::current() else { return };
    // `content_painter`, not `painter` — the scrim was already drawn by the page (see `scrim`), and
    // calling `painter` here as well would dim the screen twice.
    let p = pop().content_painter(RISE);
    let panel = panel_rect();
    pop().panel(p, panel, theme::ALERT_PANEL_RAD);

    let c = content_rect();
    draw_head(p, person, c);

    let view = viewport();
    let (max_scroll, pages) = paging(content_h(person), view.h, STEP);
    let scroll = unsafe { addr_of!(SCROLL).read() }.pos.clamp(0.0, max_scroll);

    // the two hairlines that bracket the reading block
    let rule = |y: f32| p.rrect(Rect::new(c.x, y, c.w, 1.0), 0.0, 0.0, theme::HAIRLINE);
    rule(view.y - theme::space::MD - 1.0);
    rule(view.y + view.h + theme::space::MD);

    draw_bio(p, person, view, scroll);
    // The feather is drawn AFTER the clip is released and over the same band: each edge appears
    // only when there is prose on the far side of it, which is what keeps a short biography free of
    // a permanent shadow across its first and last lines.
    widgets::edge_feather(p, view, FEATHER, scroll > 0.5, scroll < max_scroll - 0.5);
    widgets::scroll_rail(
        p,
        Rect::new(c.x + c.w - widgets::RAIL_W, view.y, widgets::RAIL_W, view.h),
        page(),
        pages,
    );

    draw_foot(p, person, c);
}

/// Eyebrow, name, identity line — stacked from the content box's top edge on `space` rungs, the
/// same flow [`head_h`] measures.
fn draw_head(p: Painter, person: &Person, c: Rect) {
    let mut y = c.y;
    widgets::tracked_run(
        p,
        &EYEBROW,
        c.x,
        y,
        theme::size::CAPTION,
        theme::TEXT_TERTIARY,
        1,
        EYEBROW_TRACK,
    );
    y += crate::text::cap_h(theme::size::CAPTION, 1) + theme::space::SM;

    // The name is ELIDED to the content box, not wrapped: this is an identity, and a two-line name
    // would push the reading window down by a whole rung of the flow. `person::refresh_runs` budgets
    // the same name to the header's own column for the same reason.
    if let Ok(cs) = CString::new(crate::text::elide(&person.name, c.w, theme::size::TITLE, 1, false)) {
        Label::new(cs.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .v(VAlign::CapTop)
            .draw(p, Rect::new(c.x, y, c.w, 0.0));
    }
    y += crate::text::cap_h(theme::size::TITLE, 1) + theme::space::SM;

    let runs = meta_runs(
        &person.roles,
        &crate::ui::fmt::pretty_date(&person.born, 0),
        &crate::ui::fmt::pretty_date(&person.died, 0),
        &person.birthplace,
    );
    if !runs.is_empty() {
        let parts: Vec<&str> = runs.iter().map(|s| s.as_str()).collect();
        let (cap_top, _) = crate::text::text_cap_band(theme::size::CAPTION, 0);
        widgets::dotted_run(
            p,
            &parts,
            c.x,
            y - cap_top,
            theme::size::CAPTION,
            theme::TEXT_SECONDARY,
            META_SEP_PAD,
        );
    }
}

/// The scrolling prose: every paragraph stacked at `-scroll`, inside a hard scissor at the viewport.
///
/// **The clip is the cut and the feather is only cosmetic** — `ui/CLAUDE.md`'s rule for a bounded
/// panel, and the arrangement its "the old edge-fade-mask trick is gone" note leaves open. Set and
/// clear are paired inside this one function, because the scissor is global GL state.
///
/// Paragraphs off the viewport are CULLED through the shared [`crate::ui::on_axis`] rather than
/// merely clipped: a `TextView::draw` submits every one of its wrapped lines, so a long biography
/// would otherwise pay for the whole document on every frame of a page transition.
fn draw_bio(p: Painter, person: &Person, view: Rect, scroll: f32) {
    let paras = paragraphs(&person.bio);
    if paras.is_empty() {
        return;
    }
    let w = text_w();
    p.clip(view);
    let mut y = view.y - scroll;
    for para in paras {
        let v = para_view(para);
        let h = v.measure_h(w);
        if crate::ui::on_axis(y - view.y, h, view.h, 0.0) {
            v.draw(p, Rect::new(view.x, y, w, 0.0));
        }
        y += h + PARA_GAP;
    }
    p.clip_clear();
}

/// The footer: what the library holds on the left, how to leave on the right, both on one centre
/// line so the keycap and the prose share a band.
fn draw_foot(p: Painter, person: &Person, c: Rect) {
    let cy = c.y + c.h - foot_h() * 0.5;
    let sz = theme::size::CAPTION;
    if let Some(line) = library_line(person.total(0), person.total(1)) {
        if let Ok(cs) = CString::new(crate::text::elide(&line, c.w * 0.5, sz, 0, false)) {
            p.text(
                cs.as_ptr(),
                c.x,
                crate::text::text_vcenter_y(sz, 0, cy),
                sz,
                theme::TEXT_TERTIARY,
                0,
                0,
            );
        }
    }
    // …and the hint, right-anchored: the widget measures itself, so the whole run ends on the
    // content box's right edge — the same edge the rail and the hairlines end on.
    let hint = widgets::KeyHint::new(HINT_PRE, HINT_KEY, HINT_POST);
    hint.draw(p, c.x + c.w - hint.width(), cy);
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// **The paging arithmetic, at every boundary that has an off-by-one in it.** The rail is drawn
    /// straight from these two numbers, so a wrong `pages` is a fill of the wrong height sitting in
    /// the wrong place — visible, but not attributable without this.
    #[test]
    fn paging_counts_resting_places_and_pins_the_last_one_to_the_end() {
        // content that fits is ONE page with no travel — the rail and both feathers vanish together
        assert_eq!(paging(300.0, 400.0, STEP), (0.0, 1));
        assert_eq!(paging(400.0, 400.0, STEP), (0.0, 1), "exactly filling is still not scrolling");

        // one short step of travel is two resting places: the top, and the end
        let (max, pages) = paging(500.0, 400.0, STEP);
        assert_eq!((max, pages), (100.0, 2));
        assert_eq!(scroll_at(1, max, STEP), 0.0);
        assert_eq!(scroll_at(2, max, STEP), 100.0, "the last page is pinned to the END of the travel…");

        // …and a step that divides exactly must not invent a page beyond it
        let (max, pages) = paging(800.0, 400.0, STEP);
        assert_eq!((max, pages), (400.0, 3), "400px of travel at 200 a step is 0 / 200 / 400");
        assert_eq!(scroll_at(3, max, STEP), 400.0);

        // a long biography, and the interior pages are whole steps
        let (max, pages) = paging(1500.0, 400.0, STEP);
        assert_eq!((max, pages), (1100.0, 7));
        assert_eq!(scroll_at(4, max, STEP), 600.0);
        assert_eq!(scroll_at(7, max, STEP), 1100.0, "the short final step lands ON the end");

        // out-of-range pages clamp instead of running off the content in either direction
        assert_eq!(scroll_at(0, max, STEP), 0.0);
        assert_eq!(scroll_at(99, max, STEP), max);
    }

    /// The rail's fill, as fractions of its track. It is quantised to PAGES — see
    /// [`widgets::rail_geom`] — so the invariant worth pinning is that the fill exactly fills the
    /// track across the pages, and that the LAST page's fill ends on the track's end.
    #[test]
    fn the_rail_fill_walks_its_track_and_ends_flush() {
        use crate::ui::widgets::rail_geom;
        // a single page is not railed at all, but the geometry still answers sanely
        assert_eq!(rail_geom(1, 1), (0.0, 1.0));

        let pages = 5;
        let (_, h) = rail_geom(1, pages);
        assert!((h - 0.2).abs() < 1e-6, "five pages ⇒ a fifth of the track");
        for page in 1..=pages {
            let (top, hh) = rail_geom(page, pages);
            assert!((hh - h).abs() < 1e-6, "every page's fill is the same height");
            assert!(top >= -1e-6 && top + hh <= 1.0 + 1e-6, "page {page} left the track");
        }
        let (top, hh) = rail_geom(pages, pages);
        assert!((top + hh - 1.0).abs() < 1e-6, "the last page's fill must end flush with the track");
        // a page index past the end clamps rather than drawing off the track
        assert_eq!(rail_geom(99, pages), rail_geom(pages, pages));
    }

    /// **The identity line's separator logic, one missing field at a time.** The whole point of
    /// building a list is that an absent fact takes its dot with it; a template with holes leaves
    /// "Actor · · London", which is the bug this shape exists to make unrepresentable.
    #[test]
    fn the_meta_line_drops_a_missing_field_and_its_separator_with_it() {
        // everything present, in flow order. BOTH dates carry their label — the design writes the
        // line as "Actor, Singer" · "Born 8 Jan 1987" · "London, England" (§1C), and the birth
        // date shipped bare here while its `Died ` sibling was labelled, which is an asymmetry
        // that reads as an oversight rather than as a decision.
        assert_eq!(
            meta_runs("Actress, Singer", "8 January 1987", "", "Stockwell, London"),
            vec!["Actress, Singer", "Born 8 January 1987", "Stockwell, London"]
        );
        // no roles — the line starts on the date, with nothing in front of it
        assert_eq!(meta_runs("", "8 January 1987", "", "London"), vec!["Born 8 January 1987", "London"]);
        // no dates at all (plex.tv knows the person but not when) — roles and place still read
        assert_eq!(meta_runs("Director", "", "", "Leeds"), vec!["Director", "Leeds"]);
        // a death date but no birth date: the run is LABELLED, because a bare second date beside a
        // birthplace would read as the birth date
        assert_eq!(meta_runs("Actor", "", "2 June 2017", "Leeds"), vec!["Actor", "Died 2 June 2017", "Leeds"]);
        // a person the provider has never heard of produces NO line, not an empty one
        assert!(meta_runs("", "", "", "").is_empty());
        // whitespace-only fields are absent too — the provider sends those
        assert!(meta_runs("  ", "\t", "", " \n ").is_empty());
    }

    /// **The footer's pluralisation, honestly.** Every arm is a different sentence, and the zero
    /// case is deliberately no sentence at all.
    #[test]
    fn the_footer_pluralises_each_kind_and_says_nothing_about_nothing() {
        assert_eq!(library_line(1, 0).as_deref(), Some("1 film in this library"));
        assert_eq!(library_line(4, 0).as_deref(), Some("4 films in this library"));
        assert_eq!(library_line(0, 1).as_deref(), Some("1 show in this library"));
        assert_eq!(library_line(0, 9).as_deref(), Some("9 shows in this library"));
        // both kinds: each is pluralised on its OWN count, which is the case a single plural flag
        // gets wrong
        assert_eq!(library_line(1, 3).as_deref(), Some("1 film and 3 shows in this library"));
        assert_eq!(library_line(2, 1).as_deref(), Some("2 films and 1 show in this library"));
        assert_eq!(library_line(1, 1).as_deref(), Some("1 film and 1 show in this library"));
        // nothing in the library is not "0 films" — a count of zero reads as a failed fetch
        assert!(library_line(0, 0).is_none());
    }

    /// Paragraphs survive the split that `TextView` would have destroyed, and the degenerate inputs
    /// (trailing newlines, a single block, nothing at all) produce no empty rows.
    #[test]
    fn the_biography_splits_into_paragraphs_and_drops_the_empty_ones() {
        assert_eq!(paragraphs("one\n\ntwo\n\nthree"), vec!["one", "two", "three"]);
        assert_eq!(paragraphs("one"), vec!["one"], "a single block is one paragraph, not none");
        // a summary that ends in blank lines must not add a page of air under the last one
        assert_eq!(paragraphs("one\n\ntwo\n\n\n\n"), vec!["one", "two"]);
        assert_eq!(paragraphs("\r\n\r\nwrapped\r\n\r\ncrlf"), vec!["wrapped", "crlf"]);
        // a SINGLE newline is not a paragraph break — it is a soft wrap in the source, and joining
        // it is exactly what the wrap does
        assert_eq!(paragraphs("one\ntwo"), vec!["one\ntwo"]);
        assert!(paragraphs("").is_empty());
        assert!(paragraphs("   \n\n  \n\n ").is_empty());
    }
}
