//! In-player Info card (mockup "Info mode"): a horizontal card over the transport with the
//! episode/movie still, title + synopsis, a metadata line with outlined capability badges, and a
//! column of action buttons. Opened from the HUD's "Info" tab; app.rs routes D-pad/OK/BACK here
//! while it's open and hides the normal transport middle behind it. Data from crate::metadata.
#![allow(dead_code)]
use crate::metadata;
use crate::ui::consts::{SCR_H, SCR_W, SDLK_DOWN, SDLK_UP};
use crate::ui::icons::Icon;
use crate::ui::popover::Popover;
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{badge, badge_w, resolve_tex_on, BadgeStyle};
use crate::ui::{Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

pub enum InfoAction {
    None,
    FromBeginning,
    GoToDetail(String), // rk to open: the show (episode) or the movie
}

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut FOCUS: c_int = 0; // index into the action-button column

/// The focused action button, for the focus probe (`crate::focusprobe`). Same reason as the other
/// panels: the card's UP/DOWN arm moves this alone, so nothing else in the log would show it.
pub(crate) fn sel() -> c_int {
    unsafe { addr_of!(FOCUS).read() }
}

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
pub(crate) fn open() {
    unsafe { addr_of_mut!(FOCUS).write(0) }
    pop().open();
}
pub(crate) fn close() {
    pop().close();
}

/// whether the playing item is an episode (→ "Go to Show") rather than a movie ("Go to Movie")
fn is_episode() -> bool {
    metadata::now_playing()
        .map(|n| n.is_episode)
        .unwrap_or(false)
}

/// the action-button labels for the playing item. An ARRAY, not a `Vec`: the count is fixed at two
/// (it is what `CTL_POP`'s const generic is sized from), and five callers ask this — one of them
/// [`draw`], every frame the card is up — so a heap allocation to hand back two `&'static str`s was
/// paid 60 times a second to learn a constant.
fn actions() -> [&'static str; 2] {
    [
        "From Beginning",
        if is_episode() {
            "Go to Show"
        } else {
            "Go to Movie"
        },
    ]
}

/// true when focus is on the last action button — a further DOWN should leave the card (back to the
/// tabs) rather than staying pinned to the bottom row
pub(crate) fn at_last() -> bool {
    let f = unsafe { addr_of!(FOCUS).read() };
    f >= actions().len() as c_int - 1
}

pub(crate) fn move_focus(sym: c_int) {
    let n = actions().len() as c_int;
    let sym = sym as u32;
    let f = unsafe { addr_of!(FOCUS).read() };
    let nf = if sym == SDLK_UP {
        (f - 1).max(0)
    } else if sym == SDLK_DOWN {
        (f + 1).min(n - 1)
    } else {
        f
    };
    unsafe { addr_of_mut!(FOCUS).write(nf) }
}

/// activate the focused action, then close
pub(crate) fn on_ok() -> InfoAction {
    let f = unsafe { addr_of!(FOCUS).read() };
    close();
    if f <= 0 {
        return InfoAction::FromBeginning;
    }
    // second action opens the show (episode) or the movie
    let rk = metadata::now_playing()
        .map(|n| n.detail_rk.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| metadata::current().map(|d| d.rk.clone()))
        .unwrap_or_default();
    if rk.is_empty() {
        InfoAction::None
    } else {
        InfoAction::GoToDetail(rk)
    }
}

/// The focused thing is a pressable CONTROL FACE — one of the card's two action buttons, rather than
/// the tab row above them. The card's `FOCUS` walks both, and is the button index only while it is
/// inside the column; the same filter [`update`] pops on, asked here so the button that dips is
/// always the button that popped.
pub(crate) fn focus_is_ctl() -> bool {
    ctl_index().is_some()
}

/// **The one filter, asked by both callers.** Which action button `FOCUS` is on, if it is in the
/// column at all — [`focus_is_ctl`] asks it to decide whether a press may dip, and [`update`] asks
/// it to decide which button `CTL_POP` pops. The doc above promised those were the same filter;
/// they were the same expression written twice, seven lines apart, in two spellings.
fn ctl_index() -> Option<usize> {
    usize::try_from(unsafe { addr_of!(FOCUS).read() })
        .ok()
        .filter(|&i| i < actions().len())
}

/// The action column's FOCUS POP — one spring per button ([`crate::ui::widgets::CtlPop`]). Two, the
/// whole of [`actions`].
static mut CTL_POP: crate::ui::widgets::CtlPop<2> = crate::ui::widgets::CtlPop::new();

pub(crate) fn update(dt: f32) {
    pop().update(dt);
    // The card's focus walks the tabs above these buttons too, and `FOCUS` is the button index only
    // while it is inside the column — outside it, every pop closes.
    unsafe { (*addr_of_mut!(CTL_POP)).step(ctl_index(), dt) };
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

/// the shared outlined chip in this panel's colours (TEXT_HEADING border/label over the card)
fn meta_badge(p: Painter, x: f32, cy: f32, text: &str) -> f32 {
    badge(
        p,
        x,
        cy,
        text,
        None,
        BadgeStyle::Outlined {
            col: theme::TEXT_HEADING,
            border: theme::OVERLAY_BORDER,
            bg: theme::SURFACE_PANEL,
        },
    )
}

/// A VIDEO codec's display name. The sibling of [`metadata::friendly_codec`], which is the one
/// AUDIO/subtitle map and spells h264 as "H264" — the dotted form is the one everybody else's
/// player uses for the video lane, and this is the only place the app names a video codec to a
/// user. An unknown codec is upper-cased rather than dropped: "Converting · AV1" is still the
/// truth, and inventing a friendly name for a codec we have never seen would not be.
pub(crate) fn video_codec_name(codec: &str) -> String {
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
/// a direct play (we pull the file), a container-only REMUX (the codecs are copied — Plex's own
/// "Direct Stream"; the mock has no case for it because its model has only "direct play" and
/// "converts", but calling a copy a conversion would state that the server re-encoded when it did
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
pub(crate) fn playback_now(
    streaming: bool,
    transcoding: bool,
    remux: bool,
    vcodec: &str,
) -> Option<String> {
    if !streaming {
        return None;
    }
    if !transcoding {
        return Some("Direct Play".to_string());
    }
    if remux {
        return Some("Direct Stream".to_string());
    }
    let name = video_codec_name(vcodec);
    // a re-encode whose output codec we somehow do not know still converted — say that much
    Some(if name.is_empty() {
        "Converting".to_string()
    } else {
        format!("Converting \u{b7} {name}")
    })
}

/// One candidate on the metadata/badge row — see [`chips_that_fit`] for why `draw` measures every
/// chip before drawing any of them, rather than drawing straight through and letting the row run
/// under the action-button column (issue #26).
struct Chip {
    label: String,
    /// What this chip IS and, for a text run, how it is inked — a data-bearing enum rather than a
    /// `kind` tag plus a `bold`/`col` pair that only ever meant something for one of the two
    /// variants: a `Badge` chip always draws through [`meta_badge`]'s own fixed style, so a flat
    /// `bold`/`col` field on every `Chip` would carry a value for `Badge` that nothing reads and
    /// that a future caller could plausibly set expecting it to matter.
    kind: ChipKind,
    /// The pixel gap this chip needs ahead of it, PROVIDED some earlier chip was actually drawn —
    /// the chip that ends up first pays nothing (see [`chips_that_fit`]'s `n == 0` case). Computed
    /// once, by [`chip_gap`], when the chip is built.
    gap_before: f32,
    /// This chip's own measured pixel width — never including `gap_before`.
    w: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum ChipKind {
    /// A plain text run (the meta line, or the live playback fact) — bold flag and ink, since the
    /// two runs on this row are inked differently (see the call site).
    Text { bold: c_int, col: [f32; 4] },
    Badge,
}

/// The row's own spacing rule between two adjacent DRAWN chips, pulled out as its own pure
/// function so this arithmetic — not just its effect through [`chips_that_fit`]'s cut — can be
/// pinned by a host test directly. `prev` is `None` for whichever chip ends up first; that chip
/// pays no gap regardless of `cur`.
fn chip_gap(prev: Option<ChipKind>, cur: ChipKind) -> f32 {
    match (prev, cur) {
        (None, _) => 0.0,
        // meta → fact: the separator is already inside the fact string (see `fact_run` below).
        (Some(ChipKind::Text { .. }), ChipKind::Text { .. }) => 0.0,
        // text block → first badge: the row's one 18px block gap.
        (Some(ChipKind::Text { .. }), ChipKind::Badge) => 18.0,
        // badge → badge: the row's 12px chip gap.
        (Some(ChipKind::Badge), ChipKind::Badge) => 12.0,
        // never happens — this row's fixed priority order puts every text run before every badge.
        (Some(ChipKind::Badge), ChipKind::Text { .. }) => 0.0,
    }
}

/// **The chip row's pure fitting maths (issue #26).** Host-testable on purpose: no `Painter`, no
/// `crate::text::text_width` — that needs a live SDL2_ttf font the host test binary never loads
/// (see `text.rs::text_width`'s own doc on why measurement is the "impure half"). `draw` measures
/// every candidate chip FIRST — meta text (genres/year/duration), then the live playback fact,
/// then the rating/audio/CC/SDH/AD badges, in that fixed priority order (see the call site for why
/// that order) — and hands the resulting `(width, gap_before)` pairs here, still in priority order,
/// as an ITERATOR rather than a collected slice: this card sits on the one route exempt from the
/// idle present gate (see `now_fact`'s own doc), so `draw` runs at ~60/s, and a temporary `Vec`
/// solely to satisfy this signature would be one more per-frame allocation this card does not need
/// — `chips.iter().map(...)` at the call site is enough. This is the one place that turns "how many
/// chips fit" into a number.
///
/// Greedy front-fill: keep adding chips while the running total (each chip's own width plus the
/// gap it needs ahead of it) still clears `avail_w`, and stop the moment one would not — dropping
/// that chip and every lower-priority one behind it, never skipping ahead to try a smaller later
/// chip instead. Skipping ahead would let a narrow low-priority badge bump a wide high-priority one
/// for no reason a viewer could predict, and would make the row's content depend on exactly how
/// much air a drop happened to leave rather than on priority alone — the row is meant to read the
/// same way every time it truncates. `avail_w` is `draw`'s `tw`: the action-button column's left
/// edge minus this panel's own text-column gutter, the same right edge the title and synopsis
/// above this row already respect. That is the actual fix for #26 — the CTA disappearing under a
/// long chip list — everything else here exists to make the cut deterministic and testable.
fn chips_that_fit(items: impl Iterator<Item = (f32, f32)>, avail_w: f32) -> usize {
    if avail_w <= 0.0 {
        return 0;
    }
    let mut used = 0.0f32;
    let mut n = 0usize;
    for (w, gap_before) in items {
        let need = if n == 0 { w } else { gap_before + w };
        if used + need > avail_w {
            break;
        }
        used += need;
        n += 1;
    }
    n
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let np = metadata::now_playing();
    let d = metadata::current();
    if np.is_none() && d.is_none() {
        return;
    }
    let p = pop().painter(0.0, 20.0); // no scrim — the card floats over the transport

    // Resolve the playing leaf's fields: `now_playing` describes the episode (show title + SxEy +
    // its still) or the movie; the loaded `Detail` backs the capability badges + genres.
    let is_ep = np.map(|n| n.is_episode).unwrap_or(false);
    let big_title = np
        .map(|n| n.title.clone())
        .or_else(|| d.map(|x| x.title.clone()))
        .unwrap_or_default();
    let ep_name = np.map(|n| n.ep_title.clone()).unwrap_or_default();
    let summary = np
        .map(|n| n.summary.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| x.summary.clone()))
        .unwrap_or_default();
    let year = np
        .map(|n| n.year)
        .or_else(|| d.map(|x| x.year))
        .unwrap_or(0);
    let dur_ms = np
        .map(|n| n.dur_ms)
        .or_else(|| d.map(|x| x.dur_ms))
        .unwrap_or(0);
    let rating = np
        .map(|n| n.rating.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| x.rating.clone()))
        .unwrap_or_default();
    let thumb_path = np
        .map(|n| n.thumb.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            d.map(|x| {
                if !x.art.is_empty() {
                    x.art.clone()
                } else {
                    x.thumb.clone()
                }
            })
        })
        .unwrap_or_default();
    // capability badges come from the PLAYING item's own tracks — `current()` is the show
    // (episode-1 streams) during a show-page episode play, or another item entirely
    let (audio, subs): (&[metadata::Stream], &[metadata::Stream]) = match metadata::playing() {
        Some(t) => (&t.audio, &t.subs),
        None => (
            d.map(|x| x.audio.as_slice()).unwrap_or(&[]),
            d.map(|x| x.subs.as_slice()).unwrap_or(&[]),
        ),
    };

    // card — tall enough that the still gets equal padding on every side (see `pad`/`sh` below)
    let cx = 80.0f32;
    let cw = SCR_W - 160.0;
    let ch = 236.0f32;
    let cyt = SCR_H - 176.0 - ch; // sit just above the Info/Chapters tabs (tabs at SCR_H-128)
    let card = Rect::new(cx, cyt, cw, ch);
    // near-opaque dark card keeps the title/synopsis legible over any scene
    let cardbg = theme::PANEL_TOP;
    p.rrect(card, 28.0, 28.0, cardbg);

    let pad = 28.0f32;
    // still (16:9), left — the *episode's* thumbnail (or the movie's landscape art). `ch` is sized
    // so (ch - sh)/2 == pad, giving the still an equal `pad` margin on every side.
    let sw = 320.0f32;
    let sh = 180.0f32;
    let sx = cx + pad;
    let sy = cyt + (ch - sh) * 0.5;
    let mut drawn = false;
    if !thumb_path.is_empty() {
        // the PLAYING item's server — the info panel describes what is on the video plane
        let t = resolve_tex_on(
            crate::route::item_sid(crate::route::cur_sid()),
            &thumb_path,
            480,
            270,
            0,
        );
        if t != 0 {
            p.tex(t, Rect::new(sx, sy, sw, sh), 16.0, theme::TINT_WHITE);
            drawn = true;
        }
    }
    if !drawn {
        p.rrect(
            Rect::new(sx, sy, sw, sh),
            16.0,
            16.0,
            theme::CARD_PLACEHOLDER,
        );
    }

    // action buttons (right column)
    let acts = actions();
    let bw = 352.0f32;
    let bh = 70.0f32;
    let bx = cx + cw - pad - bw;
    let focus = unsafe { addr_of!(FOCUS).read() };
    let total_bh = acts.len() as f32 * bh + (acts.len().saturating_sub(1)) as f32 * 16.0;
    let mut by = cyt + (ch - total_bh) * 0.5;
    let env = crate::ui::Env::inert();
    for (i, label) in acts.iter().enumerate() {
        let icon = if *label == "From Beginning" {
            Icon::Play
        } else {
            Icon::Info
        };
        if let Ok(cs) = CString::new(*label) {
            crate::ui::widgets::Button::new(
                cs.as_ptr(),
                theme::size::BODY,
                Rect::new(bx, by, bw, bh),
            )
            .icon(icon)
            .focused(i as c_int == focus)
            .scale(unsafe { addr_of!(CTL_POP).as_ref().unwrap().scale(i) })
            .draw(&env, p);
        }
        by += bh + 16.0;
    }

    // text block (between the still and the buttons): title + synopsis + tags, cap-band centred as a
    // group. Title is the playing leaf's own name (episode name / movie title) — the show-title +
    // SxEy treatment lives on the transport HUD under the playbar, not this card.
    let tx = sx + sw + 34.0;
    let tright = bx - 34.0;
    let tw = tright - tx;
    let white = theme::TEXT_PRIMARY;
    let dim = theme::TEXT_SECONDARY;

    let info_title = if is_ep {
        ep_name.clone()
    } else {
        big_title.clone()
    };
    // What the server is actually sending, straight off the resolved route — see `playback_now`.
    // `has_url`, not `!url().is_empty()`: this card sits on the one route exempt from the idle
    // present gate, so it draws at ~60/s, and `url()` clones the longest string in the app (a
    // universal-transcode `start.mkv` query is several hundred bytes) purely to test emptiness.
    // The `play_pending` half is the resolve window — see the doc.
    let now_fact = playback_now(
        crate::route::has_url() && !crate::route::play_pending(),
        crate::route::is_transcoding(),
        crate::route::is_remux(),
        &crate::route::stream_vcodec(),
    );
    // metadata line (genres · year · duration) + capability badges. Built and FIT here, before the
    // title/synopsis layout below, rather than inside its own draw block further down — see
    // `chips_that_fit`'s doc for issue #26 itself; the reason this part specifically has to happen
    // before `span` is a second, smaller bug the same fix could otherwise reintroduce. `span` folds
    // in a fixed `gap_tags + tag_h` for this row whenever it might have anything to say
    // (`has_tags`), which used to be exactly right because the row always drew SOMETHING once
    // `has_tags` was true. Once a row can be fit down to nothing at all — a single candidate (one
    // very long genre string, say) wider than `tw` on its own — `has_tags` and "this row draws a
    // pixel" stop being the same question, and centring the title/synopsis group as though a tag
    // row existed would leave a blank gap where one doesn't. So the row's actual chip COUNT, `n`,
    // has to be known before `span` is computed, not decided by a separate boolean.
    let has_tags = year > 0
        || dur_ms > 0
        || !rating.is_empty()
        || d.map(|x| !x.genres.is_empty()).unwrap_or(false)
        || !subs.is_empty()
        || audio.iter().any(|s| s.ad)
        || now_fact.is_some()
        || audio.first().and_then(|s| audio_badge(&s.codec)).is_some();

    // `has_tags` is the cheap pre-filter (a handful of comparisons, no allocation) that skips this
    // entirely for the common "nothing at all to report" case; once it's true the row still might
    // fit zero chips (see above), which is exactly what `n` then says.
    let (chips, n): (Vec<Chip>, usize) = if has_tags {
        let mut meta = Vec::new();
        if let Some(x) = d {
            for g in x.genres.iter().take(2) {
                meta.push(g.clone());
            }
        }
        if year > 0 {
            meta.push(year.to_string());
        }
        if dur_ms > 0 {
            meta.push(crate::ui::fmt::dur_short(dur_ms));
        }
        let meta_line = (!meta.is_empty()).then(|| meta.join("   \u{b7}   "));

        // …then the live playback fact, in its own quieter ink. It is a separate run rather than
        // one more entry joined into `meta` because it is a different KIND of statement — a fact
        // about this session, not a property of the item — and tertiary is what says so. No chip
        // and no capsule: a conversion that worked is not a warning.
        //
        // REGULAR weight, unlike the item run beside it, and that is the mock's: its whole meta line
        // is `font-weight:400` at `--text-tertiary`. The item run's bold/primary is this screen's own
        // long-standing deviation (the card is read over live video, not over a panel); repeating it
        // on a run the mock has no bold in would make the quiet fine print the loudest thing on the
        // row.
        //
        // The separator is baked into the STRING (rather than a stored `gap_before`) because
        // whether it is needed depends only on whether the meta line EXISTS, which is known right
        // here — not on whether the meta line ends up FITTING. By construction a chip only draws
        // once every higher-priority chip already fit (see `chips_that_fit`'s front-fill), so by
        // the time this fact chip is actually on screen those two questions have the same answer.
        let fact_run = now_fact.as_ref().map(|fact| {
            if meta_line.is_some() {
                format!("   \u{b7}   {fact}")
            } else {
                fact.clone()
            }
        });

        // pure measurement — no draw — so every candidate's width is known before any of them
        // touches the screen
        let text_w = |s: &str, bold: c_int| -> f32 {
            CString::new(s)
                .ok()
                .map(|c| crate::text::text_width(c.as_ptr(), theme::size::CAPTION, bold))
                .unwrap_or(0.0)
        };

        let mut chips: Vec<Chip> = Vec::with_capacity(7);
        let mut prev_kind: Option<ChipKind> = None;
        let mut push = |label: String, kind: ChipKind| {
            let gap_before = chip_gap(prev_kind, kind);
            let w = match kind {
                ChipKind::Text { bold, .. } => text_w(&label, bold),
                ChipKind::Badge => badge_w(&label, None),
            };
            chips.push(Chip {
                label,
                kind,
                gap_before,
                w,
            });
            prev_kind = Some(kind);
        };

        if let Some(line) = meta_line {
            push(
                line,
                ChipKind::Text {
                    bold: 1,
                    col: white,
                },
            );
        }
        if let Some(fact) = fact_run {
            push(
                fact,
                ChipKind::Text {
                    bold: 0,
                    col: theme::TEXT_TERTIARY,
                },
            );
        }
        // badges: rating (from the leaf), top-audio Dolby tag, CC/SDH/AD (from the loaded streams)
        if !rating.is_empty() {
            push(rating.clone(), ChipKind::Badge);
        }
        if let Some(tag) = audio.first().and_then(|s| audio_badge(&s.codec)) {
            push(tag, ChipKind::Badge);
        }
        if !subs.is_empty() {
            push("CC".to_string(), ChipKind::Badge);
        }
        if subs.iter().any(|s| s.sdh) {
            push("SDH".to_string(), ChipKind::Badge);
        }
        if audio.iter().any(|s| s.ad) {
            push("AD".to_string(), ChipKind::Badge);
        }

        let n = chips_that_fit(chips.iter().map(|c| (c.w, c.gap_before)), tw);
        (chips, n)
    } else {
        (Vec::new(), 0)
    };

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
    let gap_title = 6.0f32; // title → synopsis
    let gap_tags = 12.0f32; // synopsis → tags

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
    let span = title_h
        + if syn_h > 0.0 { gap_title + syn_h } else { 0.0 }
        + if n > 0 { gap_tags + tag_h } else { 0.0 };
    let mut ty = cyt + (ch - span) * 0.5;
    title_v.draw(p, Rect::new(tx, ty, tw, 0.0));
    ty += title_h;
    if syn_h > 0.0 {
        ty += gap_title;
        syn_v.draw(p, Rect::new(tx, ty, tw, 0.0));
        ty += syn_h;
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
    // promise once it crosses a border. [`chips_that_fit`] is the pure packer that turns the
    // measured candidates into a cut, so the maths is pinned by a host test with no font involved.
    if n > 0 {
        ty += gap_tags;
        let my = ty + tag_h * 0.5; // vertical centre of the tag row
        let mut mx = tx;
        for (i, chip) in chips.iter().take(n).enumerate() {
            if i > 0 {
                mx += chip.gap_before;
            }
            match chip.kind {
                ChipKind::Text { bold, col } => {
                    if let Ok(cs) = CString::new(chip.label.as_str()) {
                        let y = crate::text::text_vcenter_y(theme::size::CAPTION, bold, my);
                        p.text(cs.as_ptr(), mx, y, theme::size::CAPTION, col, 0, bold);
                    }
                }
                ChipKind::Badge => {
                    meta_badge(p, mx, my, &chip.label);
                }
            }
            mx += chip.w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{chip_gap, chips_that_fit, playback_now, ChipKind};

    /// The row's spacing rule, checked directly rather than only through [`chips_that_fit`]'s
    /// aggregate cut — `push`'s `gap_before` computation in `draw` is exactly this function, so a
    /// mistake in the match arms would otherwise only show up as a subtly wrong PIXEL position on a
    /// device, not as a failed assertion here.
    #[test]
    fn chip_gap_matches_this_rows_fixed_spacing_rule() {
        let text = ChipKind::Text {
            bold: 0,
            col: crate::ui::theme::TEXT_PRIMARY,
        };
        // whichever chip ends up first pays nothing, regardless of its own kind
        assert_eq!(chip_gap(None, text), 0.0);
        assert_eq!(chip_gap(None, ChipKind::Badge), 0.0);
        // meta → fact: the separator is baked into the fact string itself, so no extra gap
        assert_eq!(chip_gap(Some(text), text), 0.0);
        // text block → first badge: the one 18px block gap
        assert_eq!(chip_gap(Some(text), ChipKind::Badge), 18.0);
        // badge → badge: the 12px chip gap
        assert_eq!(chip_gap(Some(ChipKind::Badge), ChipKind::Badge), 12.0);
        // badge → text never happens at this row's fixed priority order, but the arm is defined
        // (see `chip_gap`'s own match) and reachable by the type, so it is pinned here too.
        assert_eq!(chip_gap(Some(ChipKind::Badge), text), 0.0);
    }

    /// **Issue #26, reproduced.** The exact geometry `draw` computes for a real card: `bx` (the
    /// action column's left edge) at 1460, this panel's own 34px gutter in front of it, and the
    /// text column starting (`tx`) at 462 — so `avail_w` below is the literal `tw` a real frame
    /// would pass to [`chips_that_fit`]. The chip list is what the issue names: an item with a
    /// meta line, a live playback fact, a rating, a premium audio tag and all three of CC/SDH/AD —
    /// "enough chips/actions... that they overlap the main CTA." Before this fix `draw` had no
    /// bound at all here, so every one of these drew and the row reached roughly 1060px into a
    /// 964px lane — squarely under the action buttons. The two assertions are the guarantee that
    /// replaces that: not everything fits, and whatever DOES fit never crosses into the CTA.
    #[test]
    fn chip_row_never_overlaps_the_action_column_with_a_long_chip_list() {
        let cta_x = 1460.0f32; // `bx` in `draw`
        let gutter = 34.0f32; // `bx - tright` in `draw`
        let text_x = 462.0f32; // `tx` in `draw`
        let avail_w = (cta_x - gutter) - text_x; // == `tw`, 964.0
        assert_eq!(avail_w, 964.0);

        // (width, gap_before) in this row's fixed priority order — meta, fact, rating, audio,
        // CC, SDH, AD — mirroring real measured widths at `size::CAPTION`.
        let items = [
            (400.0, 0.0),  // "Action, Adventure   ·   2019   ·   1h 32m"
            (170.0, 0.0),  // "   ·   Converting · HEVC" (separator baked in, so gap_before 0)
            (90.0, 18.0),  // rating badge
            (140.0, 12.0), // audio badge ("Dolby Atmos")
            (60.0, 12.0),  // CC
            (74.0, 12.0),  // SDH
            (60.0, 12.0),  // AD
        ];

        let n = chips_that_fit(items.iter().copied(), avail_w);
        assert!(
            n < items.len(),
            "this list must not all fit in {avail_w}px — that is the bug being reproduced"
        );

        let mut used = 0.0f32;
        for (i, &(w, gap)) in items.iter().take(n).enumerate() {
            used += if i == 0 { w } else { gap + w };
        }
        assert!(
            used <= avail_w,
            "drawn chips ({used}px) must never reach the action column ({avail_w}px)"
        );
    }

    /// The packer never skips ahead to a smaller lower-priority chip once a higher-priority one
    /// does not fit — dropping stops the row at that chip, full stop. Chip 1 alone leaves no room
    /// for chip 2, even though chip 2 would fit on its own in the space chip 1 wanted.
    #[test]
    fn chips_that_fit_drops_the_tail_rather_than_skipping_ahead() {
        let items = [(50.0, 0.0), (60.0, 10.0), (10.0, 10.0)];
        // avail 100: chip 0 (50) fits, chip 1 needs 10+60=70 more (total 120, too much) — stop.
        // chip 2 alone would fit in the remaining 50px, but it is never reached.
        assert_eq!(chips_that_fit(items.into_iter(), 100.0), 1);
    }

    /// A single chip wider than the whole lane draws nothing rather than spilling past `avail_w` —
    /// there is no partial chip.
    #[test]
    fn a_single_oversized_chip_is_dropped_whole() {
        assert_eq!(chips_that_fit([(2000.0, 0.0)].into_iter(), 964.0), 0);
    }

    /// Exact-fit boundary: a chip that lands EXACTLY on `avail_w` is kept, not dropped — the guard
    /// is `> avail_w`, not `>=`.
    #[test]
    fn an_exact_fit_is_kept() {
        assert_eq!(
            chips_that_fit([(100.0, 0.0), (50.0, 20.0)].into_iter(), 170.0),
            2
        );
        assert_eq!(
            chips_that_fit([(100.0, 0.0), (50.0, 21.0)].into_iter(), 170.0),
            1
        );
    }

    /// No candidates, or no room at all — both are 0, not a panic.
    #[test]
    fn empty_or_zero_width_is_zero_chips() {
        assert_eq!(chips_that_fit(std::iter::empty(), 964.0), 0);
        assert_eq!(chips_that_fit([(10.0, 0.0)].into_iter(), 0.0), 0);
        assert_eq!(chips_that_fit([(10.0, 0.0)].into_iter(), -5.0), 0);
    }

    /// The meta line's live fact, arm by arm. It is the one thing on that row the app could get
    /// wrong by GUESSING, so each arm pins a different way of guessing:
    ///   * a direct play says so and names no codec — there is no conversion to describe;
    ///   * a container remux is NOT a conversion (the codecs are copied), and calling it one would
    ///     state that the server touched the pixels when it did not — `route::is_remux`'s own doc
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
