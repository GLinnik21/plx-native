//! **The consent screen** — two switches, both off, asked once, changeable forever after.
//!
//! This is the surface every other part of the telemetry work waits on: nothing may be collected
//! before it has been shown and answered, so until it existed `telemetry::consent`'s transition
//! functions had no caller and carried `#[allow(dead_code)]` saying so. It is also the screen most
//! likely to decide what people think of the whole feature, which is why the wording below is
//! argued for rather than filled in.
//!
//! # Why it looks like this
//!
//! **Two toggle rows, not accept/decline buttons.** Crash reports and usage statistics are judged
//! as different things by the people who care about them — when Audacity retreated from its
//! telemetry it dropped usage analytics and *kept* error reporting — so one "analytics?" switch
//! would be asking a question nobody was posed. Two rows also gets symmetry for free: both start
//! off, and leaving the screen with both off is a genuine no, so there is no prominent "Yes" beside
//! a grey "No".
//!
//! **A [`Popover`], not a full-screen sheet**, per `[[ui-menu-idiom]]` — the same shape `legal.rs`
//! and the account menu use. A first-run gate is exactly where a bespoke full-screen layout would
//! have been reached for, and there is no reason for it: the content is a heading, three short
//! paragraphs and four rows.
//!
//! **The prose is first-person and short.** WP260 permits layering: the on-screen layer needs who,
//! why, that it is optional and reversible, and where to read the rest — the exhaustive Art. 13 list
//! lives in `PRIVACY.md` behind the Legal screen. A legalistic register would be worse here than a
//! plain one, and not only tonally: a solo MIT project writing like a legal department is what reads
//! as pretending to be a company, which is the thing this audience reacts to.
//!
//! **"See exactly what's sent" renders the literal payload** — Syncthing's preview, and the reason
//! the claim below it is checkable rather than reassuring. It is a real serialisation of the real
//! schema, not a mock-up: [`preview`] builds it from `diag::schema` through the same
//! `telemetry::posthog` body every send would use, so a variant added without being declared shows
//! up here.
//!
//! # BACK commits what is on screen, and does not re-ask
//!
//! Both are deliberate. Dismissing with both switches off IS the answer "no", and recording it as
//! an answer is what stops the question coming back every boot — someone who declines has decided,
//! and re-asking a decided question is the nagging pattern this design exists to avoid. Someone who
//! toggles a switch on and then presses BACK has also said what they wanted; treating that as a
//! cancellation would be second-guessing them. The screen says the decision is reversible in
//! Settings → Privacy, and it is.
//!
//! # It never appears on an automated boot
//!
//! [`should_show`] takes `dev::any_trigger_present()`, the rule `coldstart` already follows. Getting
//! this wrong would not fail loudly: `tests/run.py` injects a token and expects Home, the fps scenes
//! grade a heartbeat on a known route, and every `sim-shot` script drives a screen it chose — a
//! consent prompt in front of all of them would quietly re-point the entire harness at a screen
//! nobody wrote an assertion for.
use crate::telemetry::consent::{self, Consent};
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::KeyHint;
use crate::ui::Rect;
use std::ptr::{addr_of, addr_of_mut};

const PANEL_W: f32 = 1120.0;
const PAD: f32 = theme::alert::PAD;
const CONTENT_W: f32 = PANEL_W - 2.0 * PAD;
const EDGE_CLEAR: f32 = 68.0;
const SCRIM_A: f32 = theme::alert::SCRIM_A;
/// Reading leading, matching the Legal reader — this and that screen are the only prose in the app.
const LEAD: f32 = 42.0; // size::BODY × 1.5
/// One level of the payload preview's JSON indentation. `to_string_pretty` indents two spaces, so
/// this is per SPACE — narrow enough that four levels still leave the line room to wrap.
const INDENT_PX: f32 = 9.0;

// ---- the words -------------------------------------------------------------------------------

/// The heading. A question rather than a notice: it is a request for a favour, which is what it
/// actually is, and the honest framing also happens to be the one that does not read as a dark
/// pattern.
const TITLE: &str = "Want to help me fix bugs?";

/// Three paragraphs, in this order for a reason: who is asking, what they get, what they do not.
///
/// **"I own exactly one LG television" is the whole argument** and is literally true — it is why
/// the webOS 6 and 10 failures that reached this project cost a Cloud Test Lab slot or a reviewer's
/// patience, and why a stranger's broken set is currently invisible.
///
/// **The "not included" line is a checkable statement about the payload**, not a promise about
/// intent. An earlier draft said something closer to "I couldn't see them if I wanted to", which is
/// unverifiable and therefore worthless. This version is only sayable because `diag::schema` makes
/// it structurally true: no event variant can carry a runtime string.
const BODY: &str = "\
Hi — I'm Gleb. I build PlxNative on my own, for free, and I own exactly one LG television. When the \
app breaks on someone else's, I usually never find out.

If you switch these on, the app can tell me when it crashes, and which video formats and screens \
actually get used. That's the whole thing.

Titles, libraries, accounts and server addresses are not included in what's sent. Either way \
PlxNative works exactly the same, and you can change your mind any time in Settings → Privacy.";

const ROW_ERRORS: &str = "Tell me when it crashes";
const ROW_ERRORS_SUB: &str = "Crash and error reports.";
const ROW_USAGE: &str = "Tell me which features get used";
const ROW_USAGE_SUB: &str = "Which screens and video formats, never what you watch.";
const ROW_PREVIEW: &str = "See exactly what's sent";
const ROW_CONTINUE: &str = "Continue";

/// Row order, and the single source of it. An arm without a row, or a row without an arm, cannot
/// happen — the same shape `legal::Page::ALL` uses, and for the same reason `account_menu`'s comment
/// records: a row appended in one place and not the other is the index drift that made a menu open
/// the wrong thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowId {
    Errors,
    Usage,
    Preview,
    Continue,
}

impl RowId {
    const ALL: [RowId; 4] = [RowId::Errors, RowId::Usage, RowId::Preview, RowId::Continue];
}

// ---- state -----------------------------------------------------------------------------------

static mut POP: Popover = Popover::new();
static mut PREVIEW_POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The answer being composed. Mirrored from the stored decision at [`open`] and committed on the
/// way out — never written straight to `consent::CURRENT`, so a half-made choice cannot start
/// letting events through while the screen is still up.
static mut DRAFT: (bool, bool) = (false, false);
/// Preview scroll, in pixels. Not a spring, for `legal.rs`'s reason: a document that glides after
/// the key is released overshoots the line you stopped on.
static mut PREVIEW_SCROLL: f32 = 0.0;
/// The overflow the last [`draw_preview`] measured, so the key handler clamps against the layout
/// actually on screen rather than re-wrapping outside a draw.
static mut PREVIEW_MAX: f32 = 0.0;

#[allow(static_mut_refs)]
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
#[allow(static_mut_refs)]
fn preview_pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(PREVIEW_POP) }
}
#[allow(static_mut_refs)]
fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

pub(crate) fn is_open() -> bool {
    menu_open() || preview_open()
}
fn menu_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
fn preview_open() -> bool {
    unsafe { (*addr_of!(PREVIEW_POP)).is_open() }
}

fn draft() -> (bool, bool) {
    unsafe { *addr_of!(DRAFT) }
}

/// Should this boot put the question on screen?
///
/// Pure, and takes both inputs, so the harness rule is a host test rather than a hope — see the
/// module doc for what an automated boot landing here would do to the suite.
pub(crate) fn should_show(c: &Consent, automated: bool) -> bool {
    consent::should_ask(c, automated)
}

/// Open the question, seeded from whatever was stored. Seeded rather than always-off because a
/// policy bump re-asks somebody who already said yes, and presenting their previous answer as "no"
/// would be quietly asking them to opt in again.
pub(crate) fn open(prev: &Consent) {
    unsafe { addr_of_mut!(DRAFT).write((prev.errors, prev.usage)) };
    rebuild(0);
    pop().open();
    crate::ui::idle::invalidate();
}

/// Rebuild the rows against the current draft. Called on every toggle, because a `TableView` holds
/// its rows by value — the checkmark is state in the row, not a live read.
fn rebuild(sel: i32) {
    let (errors, usage) = draft();
    // `toggle`, not `checked`. The component was already right and the first draft picked the
    // wrong builder: `checked` is the PICKER mark — "this is the one you are on" — and an
    // unchecked row then looks exactly like an action row, which is how the capture came back with
    // "Tell me when it crashes" indistinguishable from "Continue". `toggle` states `On`/`Off` at
    // the trailing edge, which is `table.rs`'s own rule: a mark says where you are, a word says
    // what is set.
    let choices = Section::new("")
        .row(Row::new(ROW_ERRORS).detail(ROW_ERRORS_SUB).toggle(errors))
        .row(Row::new(ROW_USAGE).detail(ROW_USAGE_SUB).toggle(usage));
    let actions = Section::new("")
        .row(Row::new(ROW_PREVIEW).chevron(true))
        .row(Row::new(ROW_CONTINUE));
    // `slide: false` — a rebuild is the same list with one mark changed, and animating it as an
    // arrival would make every toggle look like a navigation.
    table().set_sections(vec![choices, actions], sel, false);
    debug_assert_eq!(RowId::ALL.len() as i32, table().n_rows());
}

/// Commit the draft and close. The one exit, used by both `Continue` and BACK — see the module doc
/// for why those are the same thing.
fn commit() {
    let (errors, usage) = draft();
    let prev = consent::current().unwrap_or_default();
    let next = consent::apply(&prev, errors, usage, || {
        crate::telemetry::mint_install_id().unwrap_or_default()
    });
    // A mint that failed leaves an empty id, which is not an identity. Refusing the opt-in is the
    // honest resolution: the alternative is inventing one from a clock or a MAC, which is precisely
    // the kind of identifier this whole design refuses.
    if next.any() && next.install_id.as_deref().unwrap_or("").is_empty() {
        crate::log("consent: no /dev/urandom — recording the answer as a refusal rather than \
                    inventing an identifier");
        crate::telemetry::record(consent::apply(&prev, false, false, String::new));
    } else {
        crate::telemetry::record(next);
    }
    // A decision can only make sending MORE restricted or newly possible, and both want a flush:
    // an opt-in drains anything this session queued, and a withdrawal is the moment the spool's
    // now-unconsented records get dropped — `flush_now` treats a record whose category is off as
    // acknowledged, so the purge happens on the same path rather than needing its own.
    crate::telemetry::flush_soon();
    close();
}

pub(crate) fn close() {
    if preview_open() {
        preview_pop().close();
    }
    if menu_open() {
        pop().close();
    }
    crate::ui::idle::invalidate();
}

/// BACK: the preview returns to the question; the question COMMITS and closes.
pub(crate) fn on_back() -> bool {
    if preview_open() {
        preview_pop().close();
        crate::ui::idle::invalidate();
        return true;
    }
    if menu_open() {
        commit();
        return true;
    }
    false
}

/// OK: toggle a switch, open the preview, or commit.
pub(crate) fn on_ok() -> bool {
    if preview_open() {
        preview_pop().close();
        crate::ui::idle::invalidate();
        return true;
    }
    if !menu_open() {
        return false;
    }
    let sel = table().sel.clamp(0, RowId::ALL.len() as i32 - 1);
    match RowId::ALL[sel as usize] {
        RowId::Errors => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((!e, u)) };
            rebuild(sel);
            crate::ui::idle::invalidate();
        }
        RowId::Usage => {
            let (e, u) = draft();
            unsafe { addr_of_mut!(DRAFT).write((e, !u)) };
            rebuild(sel);
            crate::ui::idle::invalidate();
        }
        RowId::Preview => {
            unsafe { addr_of_mut!(PREVIEW_SCROLL).write(0.0) };
            preview_pop().open();
            crate::ui::idle::invalidate();
        }
        RowId::Continue => commit(),
    }
    true
}

pub(crate) fn on_updown(delta: i32) -> bool {
    if preview_open() {
        // The pretty-printed payload is three objects and does not fit; it scrolls, like the Legal
        // reader. A third of the column per press, so a press always leaves context on screen.
        let step = (SCR_H as f32 * 0.55) * 0.33;
        unsafe {
            let max = *addr_of!(PREVIEW_MAX);
            let next = (*addr_of!(PREVIEW_SCROLL) + delta as f32 * step).clamp(0.0, max);
            if next != *addr_of!(PREVIEW_SCROLL) {
                addr_of_mut!(PREVIEW_SCROLL).write(next);
                // A clamped float has no spring behind it, so `ui::idle` cannot see the change —
                // the class of bug the present gate's note records (`Xfade` and `Spinner` both
                // shipped frozen).
                crate::ui::idle::invalidate();
            }
        }
        return true;
    }
    if menu_open() {
        table().move_sel(delta);
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

pub(crate) fn update(dt: f32) {
    pop().update(dt);
    preview_pop().update(dt);
}

// ---- the payload preview ---------------------------------------------------------------------

/// **The literal thing that would be sent**, built from the real schema through the real body
/// serialiser.
///
/// Not a mock-up, and that is the entire value: a hand-written sample drifts from the code the
/// moment anybody adds a field, and then the screen that exists to make the claim checkable is
/// itself a claim nobody checks. Because this runs `posthog::single` over every
/// [`DiagEvent`](crate::diag::schema::DiagEvent) the build can emit, an event added without being
/// declared appears here, in front of the person being asked to consent to it.
///
/// The identifier shown is a placeholder: at the moment this screen is on display no identifier
/// exists — minting one before the answer is exactly what `consent::apply` refuses to do.
pub(crate) fn preview() -> String {
    use crate::diag::schema::DiagEvent;
    let mut out = String::from(
        "Every message this app would send, in full. Nothing else is sent. The identifier is \
         random and is created only if you say yes.\n\n",
    );
    for e in [
        DiagEvent::AppLaunch,
        DiagEvent::RouteEntered { screen: "home" },
        DiagEvent::SignInCompleted,
    ] {
        let body =
            // The REAL environment this build would report, not a placeholder: it is the one
            // field on the preview that differs between a developer's build and a shipped one, and
            // showing the wrong side would make the panel lie about where the data goes.
            crate::telemetry::posthog::single(
                "<project key>",
                "<random id>",
                e,
                crate::telemetry::sender::ENVIRONMENT,
                Some("<time>"),
            );
        // **Pretty-printed, and that is not cosmetic.** Compact JSON has almost no spaces, so a
        // greedy word-wrapper sees one enormous unbreakable word, fails to fit it, and ELIDES —
        // which the first capture of this panel showed as every object trailing off in "…", cutting
        // away the very fields it exists to display. Pretty-printing gives the wrapper real break
        // opportunities and gives a reader a structure they can scan.
        let text = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
        out.push_str(&text);
        out.push_str("\n\n");
    }
    out
}

// ---- draw ------------------------------------------------------------------------------------

pub(crate) fn draw_scrim() {
    if preview_open() {
        preview_pop().scrim(SCRIM_A);
    } else if menu_open() {
        pop().scrim(SCRIM_A);
    }
}

pub(crate) fn draw() {
    if preview_open() {
        draw_preview();
    } else if menu_open() {
        draw_question();
    }
}

/// The prose, one `TextView` per paragraph.
///
/// **Not one `TextView` over the whole string**, which is what the first version did and what the
/// capture immediately showed: `TextView` collapses newlines, so three paragraphs arrived as a
/// single wall of text. `legal.rs` carries a comment recording that it made the same mistake and
/// fixed it; this made it again anyway, four files away, which is the argument for looking at the
/// picture rather than at the test result.
fn paragraphs() -> Vec<TextView<'static>> {
    BODY.split("\n\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .map(|b| TextView::new(b, theme::size::BODY, theme::TEXT_SECONDARY).leading(LEAD))
        .collect()
}

fn draw_question() {
    let p0 = pop();
    let title = TextView::new(TITLE, theme::size::HEADLINE, theme::TEXT_PRIMARY).bold();
    let title_h = title.measure_h(CONTENT_W);
    let paras = paragraphs();
    let para_hs: Vec<f32> = paras.iter().map(|t| t.measure_h(CONTENT_W)).collect();
    let prose_h: f32 = para_hs.iter().sum::<f32>()
        + theme::space::MD * (para_hs.len().saturating_sub(1)) as f32;
    let rows_h = table().measured_height();
    let h = (PAD + title_h + theme::space::MD + prose_h + theme::space::LG + rows_h + PAD)
        .min(SCR_H as f32 - 2.0 * EDGE_CLEAR);
    let r = Rect { x: (SCR_W as f32 - PANEL_W) * 0.5, y: (SCR_H as f32 - h) * 0.5, w: PANEL_W, h };

    let p = p0.content_painter(p0.appear());
    p0.panel(p, r, theme::ALERT_PANEL_RAD);

    // The question is a HEADING at the top of its own panel, not a section header inside the table.
    // The first version made it the choices section's header, and the capture showed why that was
    // wrong: the panel opened with a paragraph and no title, and the question turned up halfway
    // down as a caps label, reading as a group name rather than as what is being asked.
    let mut y = r.y + PAD;
    title.draw(p, Rect { x: r.x + PAD, y, w: CONTENT_W, h: title_h });
    y += title_h + theme::space::MD;
    for (tv, ph) in paras.iter().zip(&para_hs) {
        tv.draw(p, Rect { x: r.x + PAD, y, w: CONTENT_W, h: *ph });
        y += ph + theme::space::MD;
    }
    y += theme::space::LG - theme::space::MD; // the last paragraph already paid a gap
    table().draw(p, Rect { x: r.x + PAD, y, w: CONTENT_W, h: rows_h });
}

fn draw_preview() {
    let p0 = preview_pop();
    let h = SCR_H as f32 - 2.0 * EDGE_CLEAR;
    let r = Rect { x: (SCR_W as f32 - PANEL_W) * 0.5, y: EDGE_CLEAR, w: PANEL_W, h };
    let p = p0.content_painter(p0.appear());
    p0.panel(p, r, theme::ALERT_PANEL_RAD);

    let title = TextView::new(ROW_PREVIEW, theme::size::HEADLINE, theme::TEXT_PRIMARY).bold();
    let title_h = title.measure_h(CONTENT_W);
    title.draw(p, Rect { x: r.x + PAD, y: r.y + PAD, w: CONTENT_W, h: title_h });

    let hint = KeyHint::new(c"Press", c"BACK", c"to return");
    let hint_h = KeyHint::height() + KeyHint::pad_below();
    let body_top = r.y + PAD + title_h + theme::space::MD;
    let body_h = (r.y + h - PAD - hint_h - theme::space::MD) - body_top;

    // CAPTION rather than BODY: this is a wire format shown as evidence, not reading copy, and at
    // BODY the objects wrap so hard the structure a reader is checking stops being visible.
    //
    // One `TextView` PER LINE. Handing the whole string to one is what the first version did, and
    // `TextView` collapses newlines — so the pretty-printing above would have been thrown away and
    // the payload would have come back as the same unbreakable run that elided.
    let text = preview();
    let views: Vec<(f32, f32, TextView, f32)> = {
        let mut v = Vec::new();
        let mut y = 0.0f32;
        for line in text.lines() {
            // The wrapper trims leading space, so `to_string_pretty`'s indentation would arrive
            // flush left and the nesting that made pretty-printing worth doing would be gone. Carry
            // it as an x offset instead, which also keeps every line a real wrapping unit.
            let indent = line.len() - line.trim_start().len();
            let tv = TextView::new(line.trim_start(), theme::size::CAPTION, theme::TEXT_SECONDARY)
                .leading(34.0);
            // An empty line is a paragraph gap; it still costs its leading, which is what separates
            // the three objects.
            let lh = if line.trim().is_empty() { 34.0 } else { tv.measure_h(CONTENT_W) };
            v.push((y, lh, tv, indent as f32 * INDENT_PX));
            y += lh;
        }
        v
    };
    let full_h = views.last().map(|(y, h, _, _)| y + h).unwrap_or(0.0);
    let max = (full_h - body_h).max(0.0);
    unsafe { addr_of_mut!(PREVIEW_MAX).write(max) };
    let scroll = unsafe { (*addr_of!(PREVIEW_SCROLL)).min(max) };

    // Hard-clip and draw offset, the way `TableView::draw` and the Legal reader both do. Released
    // before returning — a leaked clip is global GL state, see `ui::guard`.
    let clip = Rect { x: r.x + PAD, y: body_top, w: CONTENT_W, h: body_h };
    p.clip(clip);
    for (by, bh, tv, ind) in &views {
        let top = clip.y - scroll + by;
        if top + bh < clip.y || top > clip.y + clip.h {
            continue;
        }
        tv.draw(p, Rect { x: clip.x + ind, y: top, w: CONTENT_W - ind, h: *bh });
    }
    p.clip_clear();

    hint.draw(p, r.x + r.w - PAD - hint.width(), r.y + h - PAD - KeyHint::height() * 0.5);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An automated boot is never asked.** The rule `coldstart` already follows, and the failure
    /// is silent: `tests/run.py` injects a token and expects Home, the fps scenes grade a heartbeat
    /// on a known route, and every `sim-shot` script drives a screen it chose.
    #[test]
    fn an_automated_boot_never_sees_the_question() {
        assert!(!should_show(&Consent::default(), true));
        assert!(should_show(&Consent::default(), false), "…but an ordinary first boot does");
    }

    /// An answered decision is not re-asked, whichever way it went. Re-asking a decided question is
    /// the nagging pattern the whole design avoids.
    #[test]
    fn an_answered_question_is_not_asked_again() {
        for (e, u) in [(false, false), (true, false), (false, true), (true, true)] {
            let answered = consent::apply(&Consent::default(), e, u, || "id".into());
            assert!(!should_show(&answered, false), "re-asked after errors={e} usage={u}");
        }
    }

    /// **The preview is the real payload.** Every event the build can emit appears in it, built
    /// through the same serialiser a send would use — so an event added without being declared
    /// shows up in front of the person being asked to consent to it, rather than in a dashboard.
    #[test]
    fn the_preview_shows_every_event_this_build_can_emit() {
        let text = preview();
        for n in crate::diag::schema::EVENT_NAMES {
            assert!(text.contains(n), "the payload preview does not show `{n}`");
        }
    }

    /// …and it shows the anonymity flag, which is the one property in the payload a reader could not
    /// otherwise verify and the one that costs a person a profile if it is ever dropped.
    #[test]
    fn the_preview_shows_the_anonymity_flag() {
        assert!(preview().contains("$process_person_profile"));
        assert!(preview().contains("false"));
    }

    /// **The preview carries no real identifier**, because at the moment it is on screen none
    /// exists — minting one before the answer is what `consent::apply` refuses to do. A preview
    /// that displayed a live id would be evidence of exactly the thing the screen denies.
    #[test]
    fn the_preview_cannot_contain_a_real_identifier() {
        let text = preview();
        assert!(text.contains("created only if you say yes"), "the intro explains the placeholder");
        assert!(text.contains("<random id>"), "and the field itself is a placeholder");
        // 32 lowercase hex in a row is what a minted id looks like; nothing here may match it.
        let bytes: Vec<char> = text.chars().collect();
        let run = bytes
            .windows(32)
            .any(|w| w.iter().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(!run, "the preview contains something shaped like a real install id");
    }

    /// The row table and the id list cannot drift: a row added to one and not the other is the
    /// index bug that makes a menu act on the wrong line.
    #[test]
    fn every_row_id_has_a_row() {
        // `DRAFT` and `TABLE` are crate globals, so this takes the shared lock rather than a local
        // one — `[[test-suite-global-pollution]]`'s rule, and the reason the whole `auth` block
        // once aborted under load.
        let _g = crate::testlock::serial();
        unsafe { addr_of_mut!(DRAFT).write((false, false)) };
        rebuild(0);
        assert_eq!(table().n_rows(), RowId::ALL.len() as i32);
    }

    /// The two labels say which switch is which without either sounding like the safe one. Pinned
    /// because the wording IS the compliance surface: granularity means a person can tell the two
    /// purposes apart.
    #[test]
    fn the_two_switches_name_two_different_purposes() {
        assert_ne!(ROW_ERRORS, ROW_USAGE);
        assert!(ROW_ERRORS_SUB.contains("Crash"));
        assert!(ROW_USAGE_SUB.contains("never what you watch"));
    }

    /// The prose carries the four things WP260's first layer needs — who, why, that it is optional,
    /// and where the rest is — plus the checkable payload claim. Asserted rather than eyeballed
    /// because a later edit for length is exactly how one of them goes missing.
    #[test]
    fn the_first_layer_says_who_why_optional_and_where() {
        assert!(BODY.contains("I'm Gleb"), "who");
        assert!(BODY.contains("when it crashes"), "why");
        assert!(BODY.contains("works exactly the same"), "optional");
        assert!(BODY.contains("Settings → Privacy"), "where to change it");
        assert!(BODY.contains("are not included in what's sent"), "the checkable payload claim");
    }
}
