//! **Track information** — the detail page's file inspector (`Alert Views.dc.html` §1B).
//!
//! One glass panel answering "what actually IS this file": the container and its size, the video
//! stream's own technicals, the Dolby Vision layering when there is any, and then EVERY audio and
//! subtitle track the part carries. It is opened with OK on the About footer's **Languages**
//! column — the block that lists those tracks — and closed by BACK; it has no controls of its own,
//! only a scroll. (It hung off a ⓘ disc in the detail hero until 2026-08-21; `detail::hero_ctls`
//! records why the door moved to the block the panel is about.)
//!
//! **Why the track list lives here and not in a panel of its own** is the design's own reasoning,
//! and it is the thing to preserve if this screen is ever split: *"the audio list belongs beside
//! the bitrates in Track information rather than in a panel of its own"*. Seven of this file's
//! eight audio tracks are Russian AC3 5.1 — language and codec alone do not tell them apart, and
//! the numbers that do are the same numbers the FILE and VIDEO columns are made of. Separated,
//! neither half answers the question.
//!
//! ## Three things here are not obvious
//!
//! **1. The feather is per-RUN, not per-pixel.** The design masks the scrolling body with an 88px
//! `linear-gradient` at whichever edge has more content behind it. This renderer has no vertical
//! mask: `fs_text_fade.frag` fades on `v_tuv.x` (a string's own width — `TextView::fade_last`'s
//! right-edge dissolve) and there is no y-axis twin. So the ramp is applied as a CASCADE ALPHA per
//! text run, evaluated at that run's cap-band centre ([`feather_alpha`]), inside a hard
//! `Painter::clip` that bounds the band exactly. The body is 100% text, so nothing escapes it.
//!
//! `table.rs` carries a warning that reads like a contradiction and is not: it replaced the track
//! menu's old fade masks with a hard scissor because *"a tall two-line edge row is cut uniformly
//! (the fade left its title bright but faded its detail line, which read as a broken clip)"*. That
//! fade was inconsistent — it dissolved some of a row's runs and not others. This one is monotone
//! in y over every run without exception, which is what a gradient IS; a label at the top of the
//! band being brighter than its own value 36px below it is the ramp working, not a broken clip.
//! The honest cost is RESOLUTION: at ~30-40px per run an 88px feather is sampled 2-3 times rather
//! than continuously. A per-pixel version means a y-axis text-fade program, which is a fill-rate
//! change on a fill-rate-bound Mali and so needs a device measurement before it is worth it — see
//! `docs/glass-hardware-budget.md`. The simulator cannot answer that question.
//!
//! **2. Codecs are named TECHNICALLY here, and that is deliberate.** `AC3`, `EAC3`, `MOV_TEXT`,
//! `HEVC` — not `metadata::friendly_codec`'s "Dolby Digital". The two surfaces are asking
//! different questions: `ui::track_menu` is a CHOOSER, where the friendly name is what a viewer
//! recognises, and this is a file INSPECTOR, where the container's own spelling is the answer. The
//! mock spells them this way for the same reason, and `track_menu`'s own trailing badge already
//! uses `codec.to_uppercase()`, so this is not a new spelling in the app.
//!
//! **3. Language names are the SERVER's, so they are not English.** The wire sends
//! `language: "Русский"` and `"Українська"` for this file; the mock's `renderVals` writes
//! "Russian" and "Ukrainian" because its author normalised them by hand. We render what the server
//! said, which is what `ui::track_menu` already does with the same tracks — the app must not name
//! one track two ways on two screens — and `appfont.ttf` (Inter) covers Cyrillic in full.
use crate::metadata::{self, Detail, Stream};
use crate::ui::consts::{SCR_H, SCR_W, SDLK_DOWN, SDLK_UP};
use crate::ui::icons::Icon;
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::popover::Popover;
use crate::ui::theme;
use crate::ui::widgets;
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

// ---- geometry (the design's numbers, `Alert Views.dc.html` §1B) --------------------------------

/// The panel frame. Fixed rather than measured: the content scrolls, so the frame is a constant of
/// the design and not a function of how many tracks a file happens to carry — a panel that grew
/// with the track count would be a different size on every item.
const PANEL_W: f32 = 1240.0;
/// **796, and it was 820 until 2026-08-22** — the design's own figure, less the 24px this panel
/// used to leave under its BACK hint. The footer closes on [`KeyHint::pad_below`] now rather than on
/// `PAD`, which is the family's change and is argued there; trimming the sheet by exactly that 24 is
/// what keeps it a trim. Every y above the footer is unchanged by construction: `fy` and the body's
/// `bottom` are both `PANEL_H - PAD - FOOTER_H - …`, so dropping 24 from the height and 24 from the
/// pad in the same edit cancels, and only the sheet's bottom edge moves.
const PANEL_H: f32 = 796.0;
/// The design places the panel at `left:340px; top:130px` in a 1920x1080 frame, which is exactly
/// centred on both axes — and clears `--glass-edge-clear` 68 on all four sides, which the spec
/// asks of every panel in the set.
const PAD: f32 = theme::alert::PAD;

/// The scroll rail: 6px wide, `--radius-pill`, one `space::MD` clear of the body's right edge.
const RAIL_W: f32 = 6.0;
const RAIL_GAP: f32 = theme::space::MD;

/// The mask's dissolve depth, from the design's `const feather = 88`.
const FEATHER: f32 = theme::alert::FEATHER;
/// One press of UP/DOWN, from the design's `tStep = 370`. A PAGE rather than a line, because the
/// body is a grid of short runs with no single row pitch to step by — and because a read-only
/// panel with no focus has nothing for a line-step to land on.
const PAGE_STEP: f32 = 370.0;

/// Scroll-spring stiffness. `220.0` is `chapters_panel`'s scroll constant — the same gesture
/// (a discrete jump the eye follows) so it gets the same rate.
const K_SCROLL: f32 = 220.0;

// Vertical rhythm inside the panel, top to bottom. Each is the design's own `margin-top`, kept as
// named steps rather than one accumulated table so a changed block moves the ones below it.
// The first four are the FAMILY's head ladder ([`theme::alert`]), not this panel's — §1B and §1C
// spell the same eyebrow → title → subtitle steps, and the one that spelled them itself drifted 12px
// tight. Named through here so the local flow still reads as a flow.
const TITLE_H: f32 = theme::alert::TITLE_LEAD;
const GAP_EYEBROW_TITLE: f32 = theme::alert::GAP_EYEBROW_TITLE;
const PATH_H: f32 = theme::size::MICRO as f32 * 1.3;
const GAP_PATH_RULE: f32 = 28.0;
const GAP_RULE_BODY: f32 = theme::space::MD;
/// The footer band is exactly the KEYCAP's own height (the design's 36), because the cap is the
/// tallest thing in it — the chevrons are 22 and the prose is `CAPTION`, both centred inside it.
///
/// Derived rather than written as a literal: it stood at a hand-tuned 32 while this panel drew its
/// cap out of `keyline_chip`, which HUGS its label's cap band. On the shared `widgets::key_cap` —
/// a fixed band, which is what the design's `82x36` KeyCap is — a 32 band would be overflowed by
/// 2px top and bottom, and the overflow would have been invisible in review because nothing draws
/// an edge there.
const FOOTER_H: f32 = crate::ui::widgets::KeyHint::height();

// Body rhythm — the design's `gap` values, one per nesting level.
const SECTION_GAP: f32 = theme::space::LG; // 40 — between the grid / AUDIO / SUBTITLES blocks
const COL_GAP: f32 = theme::space::LG; // 40 — between the three columns
const ROW_GAP: f32 = theme::space::MD; // 24 — between rows, and head → first row
const PAIR_GAP: f32 = 6.0; // label → its value
const LABEL_H: f32 = theme::size::CAPTION as f32 * 1.25;
const VALUE_H: f32 = theme::size::LABEL as f32 * 1.25;
const HEAD_H: f32 = theme::size::CAPTION as f32; // line-height:1
/// The design's `padding-bottom:40px` under the last block — air at the end of the scroll, so the
/// final row does not sit flush against the mask.
const BODY_TAIL: f32 = theme::space::LG;

/// The panel's frame on screen.
fn panel_rect() -> Rect {
    Rect::new(
        (SCR_W - PANEL_W) * 0.5,
        (SCR_H - PANEL_H) * 0.5,
        PANEL_W,
        PANEL_H,
    )
}

/// The scrolling body's viewport — inside the padding, below the header rule, above the footer
/// rule, and left of the rail.
fn body_rect() -> Rect {
    let r = panel_rect();
    let top = r.y
        + PAD
        + theme::alert::EYEBROW_LEAD
        + GAP_EYEBROW_TITLE
        + TITLE_H
        + theme::alert::GAP_TITLE_SUB
        + PATH_H
        + GAP_PATH_RULE
        + widgets::HAIRLINE_H
        + GAP_RULE_BODY;
    let bottom = r.y + PANEL_H
        - crate::ui::widgets::KeyHint::pad_below()
        - FOOTER_H
        - theme::space::MD
        - widgets::HAIRLINE_H
        - theme::space::MD;
    Rect::new(
        r.x + PAD,
        top,
        PANEL_W - 2.0 * PAD - RAIL_W - RAIL_GAP,
        bottom - top,
    )
}

// ---- the content model (PURE — this half is what the host suite grades) -------------------------

/// One `label` over its `value`, the unit the three columns are built from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Pair {
    pub(crate) label: &'static str,
    pub(crate) value: String,
}

/// One track line: a name on the left, its distinguishing detail right-aligned.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct TrackRow {
    pub(crate) name: String,
    pub(crate) detail: String,
}

/// A byte count as the design writes it — `"32.28 GB"`. **GiB, two decimals**: 34659545780 bytes
/// reads as 32.28 GB in the mock, which is `/1024³` and not `/1000³` (that would be 34.66). Plex's
/// own clients, macOS excepted, use the binary units here.
pub(crate) fn fmt_size(bytes: i64) -> Option<String> {
    if bytes <= 0 {
        return None;
    }
    const K: f64 = 1024.0;
    let b = bytes as f64;
    let (v, unit) = if b >= K * K * K {
        (b / (K * K * K), "GB")
    } else if b >= K * K {
        (b / (K * K), "MB")
    } else if b >= K {
        (b / K, "kB")
    } else {
        (b, "bytes")
    };
    Some(if unit == "bytes" {
        format!("{v:.0} bytes")
    } else {
        format!("{v:.2} {unit}")
    })
}

/// A PMS bitrate (which is in **kbps**) as `"28.9 Mbps"`, or `"640 kbps"` below a megabit. The
/// split is what keeps an audio track's 448 from reading as `0.4 Mbps`.
pub(crate) fn fmt_bitrate(kbps: i64) -> Option<String> {
    match kbps {
        k if k <= 0 => None,
        k if k >= 1000 => Some(format!("{:.1} Mbps", k as f64 / 1000.0)),
        k => Some(format!("{k} kbps")),
    }
}

/// `"HEVC (Main 10)"` — the video codec in the spelling `ui::info_panel` already uses, with its
/// profile Title-Cased in brackets. A codec with no profile is the bare name; no codec at all is
/// no row.
pub(crate) fn fmt_codec_profile(codec: &str, profile: &str) -> Option<String> {
    let name = crate::ui::info_panel::video_codec_name(codec);
    if name.is_empty() {
        return None;
    }
    let p = title_case(profile);
    Some(if p.is_empty() {
        name
    } else {
        format!("{name} ({p})")
    })
}

/// `"main 10"` → `"Main 10"`. Per WORD, and only the first character of each — upper-casing the
/// whole run would give `MAIN 10`, which reads as a second acronym beside `HEVC`.
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, w) in s.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut cs = w.chars();
        if let Some(c) = cs.next() {
            out.extend(c.to_uppercase());
            out.push_str(cs.as_str());
        }
    }
    out
}

/// `"10-bit · 4:2:0"` — bit depth and chroma as ONE fact, because neither is meaningful without
/// the other. Either half alone still draws, so a container that reports only one is not silent.
pub(crate) fn fmt_depth_chroma(bit_depth: i64, chroma: &str) -> Option<String> {
    let d = (bit_depth > 0).then(|| format!("{bit_depth}-bit"));
    let c = (!chroma.trim().is_empty()).then(|| chroma.trim().to_string());
    match (d, c) {
        (Some(d), Some(c)) => Some(format!("{d} \u{b7} {c}")),
        (Some(d), None) => Some(d),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

/// `"3840 × 1602"` — the STORED frame size, with U+00D7, never the `videoResolution` class. The
/// class for this file is the string `"4k"`, which is a badge and not a resolution: the whole
/// point of this row is that the file is 3840×1602 and not 3840×2160.
pub(crate) fn fmt_frame_size(w: i64, h: i64) -> Option<String> {
    (w > 0 && h > 0).then(|| format!("{w} \u{d7} {h}"))
}

/// `"23.976 fps"` — three decimals, trailing zeros trimmed, so 24.0 reads `"24 fps"` and 23.976
/// keeps every digit that distinguishes it from 24.
pub(crate) fn fmt_fps(fps: f64) -> Option<String> {
    if !(fps > 0.0) {
        return None;
    }
    let s = format!("{fps:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    Some(format!("{s} fps"))
}

/// `"2.35"` — the display aspect as PMS sends it, trailing zeros trimmed.
pub(crate) fn fmt_aspect(ar: f64) -> Option<String> {
    if !(ar > 0.0) {
        return None;
    }
    let s = format!("{ar:.2}");
    Some(s.trim_end_matches('0').trim_end_matches('.').to_string())
}

/// A channel layout in the design's spelling: `"5.1(side)"` → `"5.1"`, `"stereo"` → `"Stereo"`.
///
/// The parenthetical is ffmpeg's channel-ORDER note, not a different layout — `5.1(side)` and
/// `5.1` are both six channels — so it is dropped rather than shown, and a word layout is
/// capitalised because it sits mid-sentence beside a codec. Falls back to the channel COUNT when
/// the server sent no layout at all, which is the only other thing that can be said.
pub(crate) fn fmt_layout(layout: &str, channels: i64) -> String {
    let base = layout.split('(').next().unwrap_or("").trim();
    if base.is_empty() {
        return match channels {
            0 => String::new(),
            1 => "Mono".to_string(),
            2 => "Stereo".to_string(),
            n => format!("{n} ch"),
        };
    }
    // a numeric layout ("5.1", "7.1") is already how it is written; a word one is capitalised
    if base.starts_with(|c: char| c.is_ascii_digit()) {
        base.to_string()
    } else {
        title_case(base)
    }
}

/// Does this audio track carry Dolby Atmos? — [`Stream::has_atmos`], which is where the rule and
/// its evidence live now.
///
/// It moved to the data layer when the Load payload gained a second consumer for the same answer
/// (`contents.immersive`): a predicate that decides what we tell the television's pipeline is not
/// a property of the panel that happened to need it first. This stays as the free-function spelling
/// the draw code around it reads in.
pub(crate) fn has_atmos(s: &Stream) -> bool {
    s.has_atmos()
}

/// One audio track's right-hand detail: `"EAC3 5.1 + Atmos · 768 kbps · playing"`.
///
/// Every part of it exists to separate tracks a language cannot. `playing` is `Stream.selected` —
/// the server's current pick for this part, i.e. the track that will be used — and it closes the
/// line because it is the only clause about US rather than about the file.
pub(crate) fn audio_detail(s: &Stream) -> String {
    let mut head = s.codec.to_uppercase();
    let layout = fmt_layout(&s.layout, s.channels);
    if !layout.is_empty() {
        head = if head.is_empty() {
            layout
        } else {
            format!("{head} {layout}")
        };
    }
    if has_atmos(s) {
        head = if head.is_empty() {
            "Atmos".to_string()
        } else {
            format!("{head} + Atmos")
        };
    }
    let mut parts: Vec<String> = Vec::new();
    if !head.is_empty() {
        parts.push(head);
    }
    if let Some(b) = fmt_bitrate(s.bitrate) {
        parts.push(b);
    }
    if s.selected {
        parts.push("playing".to_string());
    }
    parts.join(" \u{b7} ")
}

/// The audio section's rows — the server's own `Stream[]` order, one row per track, NOT grouped.
///
/// Deliberately unlike [`subtitle_rows`], and the design says why in its own comment: *"Language
/// and codec alone do not tell seven Russian tracks apart, so the row carries the channel layout
/// and bitrate that do."* Grouping them would collapse eight distinguishable tracks into two
/// counts and destroy exactly the information the panel exists to show.
pub(crate) fn audio_rows(audio: &[Stream]) -> Vec<TrackRow> {
    audio
        .iter()
        .map(|s| TrackRow {
            name: if s.lang.trim().is_empty() {
                "Unknown".to_string()
            } else {
                s.lang.clone()
            },
            detail: audio_detail(s),
        })
        .collect()
}

/// The subtitle section's rows — identical tracks GROUPED and counted:
/// `Russian · MOV_TEXT · 6 tracks`.
///
/// The design's reasoning, verbatim: *"These ARE identical to one another — same codec, no title,
/// no forced or SDH flag — so they are counted rather than listed nine times."* Nine rows of the
/// same two words is a list you cannot read; three rows with a count is the same information.
///
/// So the grouping KEY is everything that could make two tracks different — language, codec, and
/// the three flags a viewer would pick between (`forced`, `sdh`, `external`) — plus the track's
/// own `title` where it has one. Two tracks collapse only when there is genuinely nothing left to
/// tell them apart; a forced track never merges into a full one. First-seen order is preserved,
/// which is the server's order, so the list still reads in container sequence.
pub(crate) fn subtitle_rows(subs: &[Stream]) -> Vec<TrackRow> {
    // (key, name, detail-head, count) — a Vec rather than a map so first-seen order survives; a
    // subtitle list is a handful of entries, so the linear scan is not worth a hash.
    let mut out: Vec<(String, String, String, usize)> = Vec::new();
    for s in subs {
        let name = if s.lang.trim().is_empty() {
            "Unknown".to_string()
        } else {
            s.lang.clone()
        };
        let mut head = s.codec.to_uppercase();
        for (on, tag) in [
            (s.forced, "Forced"),
            (s.sdh, "SDH"),
            (s.external, "External"),
        ] {
            if on {
                head = if head.is_empty() {
                    tag.to_string()
                } else {
                    format!("{head} \u{b7} {tag}")
                };
            }
        }
        if !s.title.trim().is_empty() && !s.title.eq_ignore_ascii_case(&name) {
            head = if head.is_empty() {
                s.title.clone()
            } else {
                format!("{head} \u{b7} {}", s.title.trim())
            };
        }
        let key = format!("{}\u{0}{}", name, head);
        match out.iter_mut().find(|e| e.0 == key) {
            Some(e) => e.3 += 1,
            None => out.push((key, name, head, 1)),
        }
    }
    out.into_iter()
        .map(|(_, name, head, n)| {
            let detail = match (head.is_empty(), n) {
                (true, 1) => String::new(),
                (true, n) => format!("{n} tracks"),
                (false, 1) => head,
                (false, n) => format!("{head} \u{b7} {n} tracks"),
            };
            TrackRow { name, detail }
        })
        .collect()
}

/// The FILE column.
pub(crate) fn file_rows(d: &Detail) -> Vec<Pair> {
    let mut v = Vec::new();
    let mut push = |label, value: Option<String>| {
        if let Some(value) = value.filter(|s| !s.is_empty()) {
            v.push(Pair { label, value });
        }
    };
    push(
        "Container",
        (!d.container.is_empty()).then(|| d.container.to_uppercase()),
    );
    push("Size", fmt_size(d.size));
    push("Total bitrate", fmt_bitrate(d.bitrate));
    push(
        "Duration",
        (d.dur_ms > 0).then(|| crate::ui::fmt::clock(d.dur_ms)),
    );
    push("Aspect ratio", fmt_aspect(d.aspect_ratio));
    v
}

/// The VIDEO column. Reads the video STREAM where the item has one and falls back to the
/// Media-level summary for the two facts that live on both — a show whose technicals were
/// backfilled from episode 1 has the summary but no stream.
pub(crate) fn video_rows(d: &Detail) -> Vec<Pair> {
    let vs = d.video.as_ref();
    let mut v = Vec::new();
    let mut push = |label, value: Option<String>| {
        if let Some(value) = value.filter(|s| !s.is_empty()) {
            v.push(Pair { label, value });
        }
    };
    let codec = vs
        .map(|s| s.codec.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(d.vcodec.as_str());
    let profile = vs.map(|s| s.profile.as_str()).unwrap_or("");
    push("Codec", fmt_codec_profile(codec, profile));
    push("Resolution", fmt_frame_size(d.width, d.height));
    push("Frame rate", fmt_fps(d.video_fps));
    // the STREAM's bitrate, not the file's — `d.bitrate` is already the FILE column's own row, and
    // repeating it here would state the same number twice under two different labels
    push("Bitrate", vs.and_then(|s| fmt_bitrate(s.bitrate)));
    push(
        "Bit depth",
        vs.and_then(|s| fmt_depth_chroma(s.bit_depth, &s.chroma)),
    );
    v
}

/// The DOLBY VISION column — **empty unless the file actually carries DV**, which is what makes
/// the third column appear on a DV item and be absent everywhere else. Every individual row is
/// still optional inside that, because "present" and "the server told us the level" are different
/// facts.
pub(crate) fn dovi_rows(d: &Detail) -> Vec<Pair> {
    if !d.dovi.present {
        return Vec::new();
    }
    let dv = &d.dovi;
    let mut v = Vec::new();
    let mut push = |label, value: Option<String>| {
        if let Some(value) = value.filter(|s: &String| !s.is_empty()) {
            v.push(Pair { label, value });
        }
    };
    push("Profile", (dv.profile > 0).then(|| dv.profile.to_string()));
    push("Level", (dv.level > 0).then(|| dv.level.to_string()));
    push("Version", dv.version_str());
    push("Base layer", dv.bl_present.then(|| "Present".to_string()));
    push("RPU", dv.rpu_present.then(|| "Present".to_string()));
    v
}

/// A section head as it is drawn: `AUDIO · 8 TRACKS`. Caps, because the head names the group.
pub(crate) fn track_head(word: &str, n: usize) -> String {
    match n {
        1 => format!("{word} \u{b7} 1 TRACK"),
        n => format!("{word} \u{b7} {n} TRACKS"),
    }
}

// ---- the scroll: pages, mask edges and the rail (also PURE) --------------------------------------

/// How many pages `content_h` of body takes in a `view_h` viewport.
///
/// **Not the design's hardcoded `tPages = 3`** — that number is the mock's own content measured by
/// hand, and a panel driven by live data has a different height on every item. The last page is
/// defined to land FLUSH on the bottom of the content (see [`scroll_for`]), so this is the number
/// of `PAGE_STEP` jumps needed to get there, plus the page you start on. Always ≥ 1, so content
/// that fits still reports one page and the rail fills completely.
pub(crate) fn pages_for(content_h: f32, view_h: f32) -> i32 {
    let over = content_h - view_h;
    if !(over > 0.5) || view_h <= 0.0 {
        return 1;
    }
    1 + (over / PAGE_STEP).ceil() as i32
}

/// The body's scroll offset (px, ≥ 0) on 1-based `page`.
///
/// Clamped to the overflow, which is what makes the LAST page show the end of the content rather
/// than `(pages-1) * PAGE_STEP` of whitespace past it. That clamp is also why [`pages_for`] can
/// round up freely.
pub(crate) fn scroll_for(page: i32, content_h: f32, view_h: f32) -> f32 {
    let over = (content_h - view_h).max(0.0);
    (((page - 1).max(0) as f32) * PAGE_STEP).clamp(0.0, over)
}

/// Which mask edges are feathered on `page` of `pages`: `(top, bottom)`.
///
/// Straight from the design's `mask()`: a head gradient only when there is content ABOVE
/// (`page > 1`) and a tail one only when there is content BELOW (`page < pages`). The two are
/// independent, so a middle page wears both and a single-page body wears neither — which is the
/// whole affordance: the feather IS the statement that the list continues.
pub(crate) fn mask_edges(page: i32, pages: i32) -> (bool, bool) {
    (page > 1, page < pages)
}

/// The rail's fill as `(top_fraction, height_fraction)` of the track — the design's
/// `railTop = (page-1)/pages` and `railH = 1/pages`.
///
/// Note it is keyed on the PAGE, not on the pixel scroll: the fill is a page indicator, so it
/// steps with the press and its height states how much of the whole the viewport is showing.
pub(crate) fn rail_fill(page: i32, pages: i32) -> (f32, f32) {
    let pages = pages.max(1) as f32;
    let page = page.max(1) as f32;
    (
        ((page - 1.0) / pages).clamp(0.0, 1.0),
        (1.0 / pages).clamp(0.0, 1.0),
    )
}

/// The feather's cascade alpha for a run whose cap band is centred at `y`, given the body band
/// `[top, bottom]` and which edges are active.
///
/// Smoothstepped rather than linear so the dissolve eases at both ends of the ramp — a linear
/// ramp has a visible corner where it meets full opacity. Both edges are evaluated and the SMALLER
/// wins, so a body short enough for the two 88px bands to overlap dissolves from both sides
/// instead of one edge cancelling the other.
pub(crate) fn feather_alpha(y: f32, top: f32, bottom: f32, edges: (bool, bool)) -> f32 {
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut a = 1.0f32;
    if edges.0 {
        a = a.min(smooth((y - top) / FEATHER));
    }
    if edges.1 {
        a = a.min(smooth((bottom - y) / FEATHER));
    }
    a
}

// ---- state --------------------------------------------------------------------------------------

static mut POP: Popover = Popover::new();
/// 1-based page. The panel's whole cursor: it has no focusable control, so there is nothing else
/// a key press can move.
static mut PAGE: c_int = 1;
/// The body's scroll offset in px, sprung toward [`scroll_for`]. A `Spring` and not a raw value
/// because that is what reports the motion to `ui::idle` — `gfx::spring` is one of the two
/// integrators the frame gate watches, so a scroll animated any other way would freeze mid-glide
/// on a settled screen (the trap that shipped `Xfade` and `Spinner` frozen).
static mut SCROLL: Spring = Spring::at(0.0);
/// The content height measured at the last draw, so `update` can spring toward a target and
/// `move_focus` can clamp the page without re-measuring text off the draw path (`text_width`
/// reaches SDL_ttf, which the host suite cannot link).
static mut CONTENT_H: f32 = 0.0;

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// The current page, for the focus probe (`crate::focusprobe`) — this panel's only cursor, and the
/// UP/DOWN arm moves it and nothing else, so nothing in the log would show it otherwise.
pub(crate) fn sel() -> c_int {
    unsafe { addr_of!(PAGE).read() }
}

/// Is there a file for this panel to describe?
///
/// **The gate for the Languages column's press**, and it is `part` rather than `is_show` on
/// purpose. A SHOW
/// container carries no `Media` of its own — `parse_streams` backfills its audio/subtitle lists
/// from episode 1 for the About footer — so on a show page this panel would print episode 1's
/// path, size and bitrate under the show's name. That is not a truncation of the truth, it is a
/// different file, and `Detail::part` is exactly the field that is empty when the item has no file
/// of its own (`metadata.rs`: "Media[0].Part[0].key for a leaf (movie/episode); empty for a show").
///
/// It is also false during the whole mount fetch, which costs nothing: the About footer is drawn
/// from a loaded item, so there is no frame on which the Languages column is on screen and this is
/// still false. Unlike the hero disc it replaced, the column does not appear or vanish with the
/// answer — it is always the third of four — so a press that arrives early is refused rather than
/// landing on a control that has moved.
pub(crate) fn is_available() -> bool {
    metadata::current()
        .map(|d| !d.part.is_empty())
        .unwrap_or(false)
}

pub(crate) fn open() {
    unsafe {
        addr_of_mut!(PAGE).write(1);
        addr_of_mut!(SCROLL).write(Spring::at(0.0));
    }
    pop().open();
    // a panel appearing is a discrete change no spring reports on the frame it happens
    crate::ui::idle::invalidate();
}

pub(crate) fn close() {
    pop().close();
    crate::ui::idle::invalidate();
}

/// Jump straight to 1-based `page` — the headless door, for `/tmp/plxnative-tracks=<n>`.
///
/// Deliberately does NOT jump the scroll spring: the page is the state and `update` springs the
/// body toward it, which is the same path a key press takes. A capture taken a frame after this
/// would show the glide; every recipe that uses it lets the panel settle first.
pub(crate) fn set_page(n: c_int) {
    unsafe { addr_of_mut!(PAGE).write(n.max(1)) };
    crate::ui::idle::invalidate();
}

/// UP/DOWN page the body. Returns nothing: there is no action to report, and BACK is the only
/// other key this panel answers.
pub(crate) fn move_focus(sym: c_int) {
    let sym = sym as u32;
    let content = unsafe { addr_of!(CONTENT_H).read() };
    let pages = pages_for(content, body_rect().h);
    let p = unsafe { addr_of!(PAGE).read() };
    let np = if sym == SDLK_UP {
        (p - 1).max(1)
    } else if sym == SDLK_DOWN {
        (p + 1).min(pages)
    } else {
        p
    };
    unsafe { addr_of_mut!(PAGE).write(np) };
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let view = body_rect().h;
    let content = unsafe { addr_of!(CONTENT_H).read() };
    // Re-clamp: the item can be replaced under an open panel (a refresh landing), and a page past
    // the new end would spring toward a scroll offset that shows nothing.
    //
    // Guarded on having MEASURED something, because `update` runs before the first `draw` and
    // `CONTENT_H` is 0 until one has: clamping against an unmeasured body would say "one page"
    // and silently undo a page set before the first frame — which is exactly what
    // `/tmp/plxnative-tracks=<n>` does.
    let mut page = unsafe { addr_of!(PAGE).read() };
    if content > 0.0 {
        page = page.clamp(1, pages_for(content, view));
        unsafe { addr_of_mut!(PAGE).write(page) };
    }
    let target = scroll_for(page, content, view);
    let sc = unsafe { &mut *addr_of_mut!(SCROLL) };
    sc.step(target, K_SCROLL, dt);
    crate::ui::anim::probe("tracks.scroll", sc.pos, sc.vel, target, dt);
}

// ---- draw ---------------------------------------------------------------------------------------

/// A vertical cursor through the scrolling body: it lays runs out top-down, applies the feather's
/// alpha per run, and — when `measure` is set — walks the same flow without drawing so the content
/// height is produced by the very code that draws it.
struct Flow {
    p: Painter,
    /// screen y of the next block's top, already scrolled
    y: f32,
    /// the body viewport, for the feather and the cull
    band: Rect,
    edges: (bool, bool),
    measure: bool,
    /// content height accumulated in CONTENT space (unscrolled), for `CONTENT_H`
    h: f32,
}

impl Flow {
    fn advance(&mut self, dy: f32) {
        self.y += dy;
        self.h += dy;
    }
    /// The cascade alpha for a run occupying `[self.y, self.y + h]`, sampled at its cap-band
    /// centre — the ONE place the feather is applied, so no run can escape it.
    fn alpha_at(&self, h: f32) -> f32 {
        feather_alpha(
            self.y + h * 0.5,
            self.band.y,
            self.band.y + self.band.h,
            self.edges,
        )
    }
    /// Draw one run at the cursor and advance past it. `w` is the run's own column width, which is
    /// also its elide budget; `align` follows `Label`'s.
    fn run(
        &mut self,
        s: &str,
        x: f32,
        w: f32,
        h: f32,
        sz: c_int,
        bold: bool,
        col: [f32; 4],
        align: HAlign,
    ) {
        if !self.measure && self.visible(h) {
            let a = self.alpha_at(h);
            if a > 0.002 {
                // elide by CHARACTER, which `text::elide` does — the file path and every language
                // name here can be non-ASCII (`"Українська"`), and a byte-wise cut would split a
                // UTF-8 sequence
                let cut = crate::text::elide(s, w, sz, bold as c_int, false);
                if let Ok(cs) = CString::new(cut) {
                    let mut l = Label::new(cs.as_ptr(), sz, col).v(VAlign::CapTop).h(align);
                    if bold {
                        l = l.bold();
                    }
                    l.draw(self.p.alpha(a), Rect::new(x, self.y, w, h));
                }
            }
        }
        self.advance(h);
    }
    /// Is a block at the cursor inside the viewport at all? The shared cull test, so a long track
    /// list costs only the rows on screen — `on_axis` is the app's ONE off-axis predicate.
    fn visible(&self, h: f32) -> bool {
        crate::ui::on_axis(self.y - self.band.y, h, self.band.h, 0.0)
    }
}

/// A caps section head — `FILE`, `AUDIO · 8 TRACKS`.
///
/// `to_uppercase`, not `to_ascii_uppercase`, matching `table.rs`'s header rule: a head here can
/// carry any script the server sends.
///
/// **The design sets `--track-caps` tracking on this head and we do not draw it — and the reason
/// recorded here was that the renderer HAS no letter-spacing, which is no longer true.**
/// `widgets::tracked_run` is a shared component (the PLEX PASS capsule's per-character technique,
/// promoted), and the person bio's own eyebrow spends it. So the three panels' kickers do not
/// match each other: §1C is tracked, §1A and this one are not. Left as it is deliberately rather
/// than guessed at — the spec copy in this tree carries no `ds/` token files, so `--track-caps`'s
/// actual value is unverifiable here and §1C's `.08em` is itself a guess. Settle the number with
/// the designer and then move all three together; caps + bold at CAPTION is meanwhile the app's
/// established head face.
fn head(f: &mut Flow, text: &str, x: f32, w: f32) {
    f.run(
        &text.to_uppercase(),
        x,
        w,
        HEAD_H,
        theme::size::CAPTION,
        true,
        theme::TEXT_TERTIARY,
        HAlign::Left,
    );
}

/// One label-over-value pair.
fn pair(f: &mut Flow, pr: &Pair, x: f32, w: f32) {
    f.run(
        pr.label,
        x,
        w,
        LABEL_H,
        theme::size::CAPTION,
        false,
        theme::TEXT_TERTIARY,
        HAlign::Left,
    );
    f.advance(PAIR_GAP);
    f.run(
        &pr.value,
        x,
        w,
        VALUE_H,
        theme::size::LABEL,
        true,
        theme::TEXT_HEADING,
        HAlign::Left,
    );
}

/// One track line — the name left, its detail right, on ONE baseline. Drawn as two runs into the
/// same band rather than through two `Flow::run` calls, so the two halves cannot pick up different
/// feather alphas and read as the broken clip `table.rs` warns about.
fn track_line(f: &mut Flow, row: &TrackRow, x: f32, w: f32) {
    const NAME_FRAC: f32 = 0.42;
    if !f.measure && f.visible(VALUE_H) {
        let a = f.alpha_at(VALUE_H);
        if a > 0.002 {
            let pa = f.p.alpha(a);
            let nw = w * NAME_FRAC;
            if let Ok(cs) = CString::new(crate::text::elide(
                &row.name,
                nw,
                theme::size::LABEL,
                1,
                false,
            )) {
                Label::new(cs.as_ptr(), theme::size::LABEL, theme::TEXT_HEADING)
                    .bold()
                    .v(VAlign::CapTop)
                    .draw(pa, Rect::new(x, f.y, nw, VALUE_H));
            }
            let dw = w - nw - theme::space::MD;
            if !row.detail.is_empty() {
                // the detail sits on the NAME's baseline (`align-items:baseline` in the design),
                // which at a smaller rung is not the same cap-top — `baseline_y` is the helper
                // that exists so this is not a hand-tuned offset
                let dy =
                    crate::text::baseline_y(theme::size::CAPTION, 0, theme::size::LABEL, 1, f.y);
                if let Ok(cs) = CString::new(crate::text::elide(
                    &row.detail,
                    dw,
                    theme::size::CAPTION,
                    0,
                    false,
                )) {
                    Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
                        .h(HAlign::Right)
                        .v(VAlign::CapTop)
                        .draw(
                            pa,
                            Rect::new(x + w - dw, dy, dw, theme::size::CAPTION as f32),
                        );
                }
            }
        }
    }
    f.advance(VALUE_H);
}

/// Everything the body draws, built ONCE per frame.
///
/// The body is walked TWICE — once to measure (which is what the page count and the rail are
/// derived from) and once to draw — and the model is the expensive half: ~20 `String`s across the
/// columns plus one per track. Building it per PASS meant allocating all of that twice for every
/// presented frame of a scroll. It is still rebuilt per frame rather than cached, which is the
/// codebase's normal immediate-mode shape and needs no invalidation when the item is replaced
/// under an open panel.
struct Content {
    /// the three columns in drawn order; an EMPTY one is absent from the grid, not blank
    cols: [(&'static str, Vec<Pair>); 3],
    /// `(track total, grouped rows)` per section — the two are different numbers for subtitles
    audio: (usize, Vec<TrackRow>),
    subs: (usize, Vec<TrackRow>),
}

fn content_of(d: &Detail) -> Content {
    Content {
        cols: [
            ("FILE", file_rows(d)),
            ("VIDEO", video_rows(d)),
            ("DOLBY VISION", dovi_rows(d)),
        ],
        audio: (d.audio.len(), audio_rows(&d.audio)),
        subs: (d.subs.len(), subtitle_rows(&d.subs)),
    }
}

/// Walk the whole body once — measuring when `measure`, drawing otherwise. ONE function for both
/// so the height the rail and the page count are computed from is by construction the height the
/// draw produces.
fn body_flow(
    c: &Content,
    band: Rect,
    scroll: f32,
    edges: (bool, bool),
    p: Painter,
    measure: bool,
) -> f32 {
    let mut f = Flow {
        p,
        y: band.y - scroll,
        band,
        edges,
        measure,
        h: 0.0,
    };
    let cw = (band.w - 2.0 * COL_GAP) / 3.0;

    // ---- the three columns. They are a GRID, so each starts at the band top and the block's
    // height is the tallest of them — walked column by column with the cursor reset, which is the
    // one place `Flow`'s single cursor does not describe the layout.
    let cols = &c.cols;
    let top = f.y;
    let mut tallest = 0.0f32;
    for (i, (title, rows)) in cols.iter().enumerate() {
        if rows.is_empty() {
            continue; // DOLBY VISION on a file that has none: the column is absent, not empty
        }
        let x = band.x + i as f32 * (cw + COL_GAP);
        f.y = top;
        f.h = 0.0;
        head(&mut f, title, x, cw);
        for pr in rows {
            f.advance(ROW_GAP);
            pair(&mut f, pr, x, cw);
        }
        tallest = tallest.max(f.h);
    }
    f.y = top + tallest;
    f.h = tallest;

    // ---- the two track sections. The head counts TRACKS and the body counts ROWS, and for
    // subtitles those are different numbers — that is the whole point of grouping them, and
    // passing `rows.len()` here is how the head came to say "SUBTITLES · 3 TRACKS" over a list
    // describing nine of them.
    for (word, (total, rows)) in [("AUDIO", &c.audio), ("SUBTITLES", &c.subs)] {
        if rows.is_empty() {
            continue;
        }
        f.advance(SECTION_GAP);
        head(&mut f, &track_head(word, *total), band.x, band.w);
        for row in rows {
            f.advance(ROW_GAP);
            track_line(&mut f, row, band.x, band.w);
        }
    }
    f.advance(BODY_TAIL);
    f.h
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let Some(d) = metadata::current() else { return };
    let r = panel_rect();
    // CACHED glass over a page that is standing still, so the scrim rides `painter` and lands in
    // the snapshot with it — the pairing `Popover::scrim`'s doc reserves for a DYNAMIC backdrop is
    // not this panel's. `alt_sources` is the worked example on this same page.
    let p = pop().painter(SCRIM_A, Popover::RISE);
    pop().panel(p, r, theme::ALERT_PANEL_RAD);

    // ---- header ------------------------------------------------------------------------------
    let cx = r.x + PAD;
    let cw = PANEL_W - 2.0 * PAD;
    let mut y = r.y + PAD;
    let eyebrow = c"TRACK INFORMATION";
    Label::new(eyebrow.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(cx, y, cw, theme::alert::EYEBROW_LEAD));
    y += theme::alert::EYEBROW_LEAD + GAP_EYEBROW_TITLE;
    if let Ok(cs) = CString::new(crate::text::elide(
        &d.title,
        cw,
        theme::size::TITLE,
        1,
        false,
    )) {
        Label::new(cs.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .v(VAlign::CapTop)
            .draw(p, Rect::new(cx, y, cw, TITLE_H));
    }
    y += TITLE_H + theme::alert::GAP_TITLE_SUB;
    // the server's own path for the part. One line, ellipsised, micro, tertiary — it is the
    // panel's subject, not its content, and it can be arbitrarily long.
    if !d.file.is_empty() {
        if let Ok(cs) = CString::new(crate::text::elide(
            &d.file,
            cw,
            theme::size::MICRO,
            0,
            false,
        )) {
            Label::new(cs.as_ptr(), theme::size::MICRO, theme::TEXT_TERTIARY)
                .v(VAlign::CapTop)
                .draw(p, Rect::new(cx, y, cw, PATH_H));
        }
    }
    y += PATH_H + GAP_PATH_RULE;
    rule(p, cx, y, cw);

    // ---- body --------------------------------------------------------------------------------
    let band = body_rect();
    let model = content_of(d);
    // measure first, off the same walk the draw uses, so the page count and the rail describe the
    // content actually about to be drawn rather than the previous item's
    let content = body_flow(&model, band, 0.0, (false, false), p, true);
    unsafe { addr_of_mut!(CONTENT_H).write(content) };
    let pages = pages_for(content, band.h);
    let page = unsafe { addr_of!(PAGE).read() }.clamp(1, pages);
    let edges = mask_edges(page, pages);
    let scroll = unsafe { addr_of!(SCROLL).read() }.pos;
    // the hard bound. The feather dissolves runs approaching the edge; this is what guarantees
    // nothing is painted outside the band at all, including a run the ramp has not reached yet.
    p.clip(band);
    body_flow(&model, band, scroll, edges, p, false);
    p.clip_clear();

    // ---- the rail ----------------------------------------------------------------------------
    // Drawn whatever the page count: a single-page body gets a full-height fill, which states
    // "this is all of it" rather than leaving the reader to infer it from an absent control.
    let rx = r.x + PANEL_W - PAD - RAIL_W;
    let track = Rect::new(rx, band.y, RAIL_W, band.h);
    let rad = RAIL_W * 0.5; // --radius-pill: half the short side, which is how this SDF spells it
    p.rrect(track, rad, rad, theme::RAIL_TRACK);
    let (ft, fh) = rail_fill(page, pages);
    p.rrect(
        Rect::new(rx, band.y + band.h * ft, RAIL_W, band.h * fh),
        rad,
        rad,
        theme::RAIL_FILL,
    );

    // ---- footer ------------------------------------------------------------------------------
    let fy = r.y + PANEL_H - crate::ui::widgets::KeyHint::pad_below() - FOOTER_H;
    rule(p, cx, fy - theme::space::MD - widgets::HAIRLINE_H, cw);
    let cy = fy + FOOTER_H * 0.5;
    // left: the two chevrons + "to scroll". Marks, not controls — there is nothing to focus, so
    // they are drawn at their natural size with no frame (the Library A–Z rail's idiom).
    const GLYPH: f32 = 22.0;
    let mut gx = cx;
    for icon in [Icon::ChevronUp, Icon::ChevronDown] {
        crate::ui::icons::draw(
            p,
            icon,
            Rect::new(gx, cy - GLYPH * 0.5, GLYPH, GLYPH),
            theme::TEXT_TERTIARY,
        );
        gx += GLYPH + theme::space::XS + 4.0;
    }
    // The design's `gap:12` applies between EVERY item in this run, the label included — the loop
    // above already advances by 12, so the label takes the pen where it is rather than adding a
    // second nudge of its own (which is what made this one gap 16 while its neighbour was 12).
    let hint = c"to scroll";
    Label::new(hint.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .draw(p, Rect::new(gx, fy, cw, FOOTER_H));
    // right: Press [BACK] to return, RIGHT-aligned on the padding edge. The shared
    // `widgets::KeyHint` places by x and each caller solves its own alignment, so a right edge is
    // `right - width()`. (This was a local `key_cap_hint` that built its cap out of `keyline_chip`
    // — which HUGS its label's cap band, where the design's KeyCap is a fixed 82x36 with a MICRO
    // bold label. The shared cap is the fixed band, so the panels all draw one object.)
    let back_hint = crate::ui::widgets::KeyHint::new(c"Press", c"BACK", c"to return");
    back_hint.draw(p, cx + cw - back_hint.width(), cy);
}

/// The panel's one-pixel divider — `widgets::hairline`, which carries the rule this file wrote
/// down (a `theme::HAIRLINE` rect, never a 1px `rrect`) and now enforces for the whole family.
fn rule(p: Painter, x: f32, y: f32, w: f32) {
    widgets::hairline(p, x, y, w);
}

/// The modal dim, from the design's `scrimStill: 0.46`.
const SCRIM_A: f32 = theme::alert::SCRIM_A;

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(lang: &str, codec: &str, layout: &str, ch: i64, kbps: i64) -> Stream {
        Stream {
            lang: lang.into(),
            codec: codec.into(),
            layout: layout.into(),
            channels: ch,
            bitrate: kbps,
            ..Default::default()
        }
    }
    fn sub(lang: &str, codec: &str) -> Stream {
        Stream {
            lang: lang.into(),
            codec: codec.into(),
            ..Default::default()
        }
    }

    /// **The eight audio rows of `/library/metadata/5`, byte for byte against the design's
    /// `renderVals`.** This is the fixture the whole panel was drawn from, so it is the one place
    /// the formatter, the layout cleanup, the Atmos test and the `playing` clause are graded
    /// TOGETHER — each of them is individually plausible and the composition is what the mock
    /// actually specifies.
    #[test]
    fn the_audio_rows_reproduce_the_designs_own_content_for_wicked() {
        let mut atmos = audio("English", "eac3", "5.1(side)", 6, 768);
        atmos.profile = "dolby digital plus + dolby atmos".into();
        atmos.selected = true;
        let streams = vec![
            audio("Русский", "ac3", "5.1(side)", 6, 448),
            audio("Русский", "ac3", "5.1(side)", 6, 640),
            audio("Русский", "ac3", "5.1(side)", 6, 384),
            audio("Русский", "eac3", "5.1(side)", 6, 768),
            audio("Русский", "ac3", "5.1(side)", 6, 384),
            audio("Русский", "ac3", "stereo", 2, 384),
            audio("Русский", "ac3", "stereo", 2, 320),
            atmos,
        ];
        let rows = audio_rows(&streams);
        let details: Vec<&str> = rows.iter().map(|r| r.detail.as_str()).collect();
        assert_eq!(
            details,
            vec![
                "AC3 5.1 \u{b7} 448 kbps",
                "AC3 5.1 \u{b7} 640 kbps",
                "AC3 5.1 \u{b7} 384 kbps",
                "EAC3 5.1 \u{b7} 768 kbps",
                "AC3 5.1 \u{b7} 384 kbps",
                "AC3 Stereo \u{b7} 384 kbps",
                "AC3 Stereo \u{b7} 320 kbps",
                "EAC3 5.1 + Atmos \u{b7} 768 kbps \u{b7} playing",
            ]
        );
        // …and the NAME is the server's own language string, not an English normalisation of it
        assert_eq!(rows[0].name, "Русский");
        assert_eq!(rows[7].name, "English");
        // eight tracks stay eight rows — the audio list is deliberately NOT grouped
        assert_eq!(
            rows.len(),
            8,
            "grouping audio would destroy what tells the seven rus tracks apart"
        );
    }

    /// Atmos is found in `profile` and NOWHERE else, which is the fact a live probe settled and
    /// the one a reasonable implementation gets wrong: the Atmos track's `audioChannelLayout` is
    /// the ordinary `"5.1(side)"` and its `title` is empty, so a client testing either badges
    /// nothing at all — silently, and on every file.
    #[test]
    fn atmos_is_detected_from_the_profile_and_not_from_the_layout_or_title() {
        let mut s = audio("English", "eac3", "5.1(side)", 6, 768);
        assert!(!has_atmos(&s), "the layout alone must not imply Atmos");
        s.title = "Atmos".into();
        assert!(
            !has_atmos(&s),
            "a track TITLE is a user string, not the structured answer"
        );
        s.title = String::new();
        s.profile = "dolby digital plus + dolby atmos".into();
        assert!(has_atmos(&s));
        // …case-insensitively, since PMS's own casing is lower and nothing promises it stays
        s.profile = "Dolby Digital Plus + Dolby ATMOS".into();
        assert!(has_atmos(&s));
    }

    /// **The subtitle list is GROUPED and counted** (the design's own note), and the grouping key
    /// is everything that could tell two tracks apart. Nine tracks → three rows for this file.
    #[test]
    fn identical_subtitle_tracks_are_counted_rather_than_listed() {
        let mut subs: Vec<Stream> = (0..6).map(|_| sub("Русский", "mov_text")).collect();
        subs.push(sub("English", "mov_text"));
        subs.push(sub("English", "mov_text"));
        subs.push(sub("Українська", "mov_text"));
        let rows = subtitle_rows(&subs);
        assert_eq!(
            rows.len(),
            3,
            "nine identical-by-language tracks are three rows"
        );
        assert_eq!(rows[0].name, "Русский");
        assert_eq!(rows[0].detail, "MOV_TEXT \u{b7} 6 tracks");
        assert_eq!(rows[1].detail, "MOV_TEXT \u{b7} 2 tracks");
        // a lone track carries NO count — "1 tracks" is wrong and "· 1 track" is noise
        assert_eq!(rows[2].name, "Українська");
        assert_eq!(rows[2].detail, "MOV_TEXT");
        // …and the head states the TRUE track total, not the row count. These are DIFFERENT
        // numbers for subtitles and equal for audio, which is exactly why the wrong one shipped:
        // it is invisible on the section that is not grouped.
        assert_eq!(
            track_head("SUBTITLES", subs.len()),
            "SUBTITLES \u{b7} 9 TRACKS"
        );
        assert_ne!(
            track_head("SUBTITLES", rows.len()),
            track_head("SUBTITLES", subs.len()),
            "the grouped ROW count is not the track count — the head must take the latter"
        );
        assert_eq!(
            track_head("AUDIO", 1),
            "AUDIO \u{b7} 1 TRACK",
            "singular, not '1 TRACKS'"
        );
    }

    /// A track that differs in any way a viewer would pick between must NOT be merged away. This
    /// is the half of the grouping rule that a count-by-language implementation gets wrong, and it
    /// is silent: a forced subtitle folded into the full one simply disappears from the list.
    #[test]
    fn a_forced_or_sdh_track_never_merges_into_a_plain_one() {
        let mut forced = sub("English", "subrip");
        forced.forced = true;
        let mut sdh = sub("English", "subrip");
        sdh.sdh = true;
        let rows = subtitle_rows(&[
            sub("English", "subrip"),
            forced,
            sdh,
            sub("English", "subrip"),
        ]);
        assert_eq!(rows.len(), 3, "plain×2, forced, SDH");
        assert_eq!(rows[0].detail, "SUBRIP \u{b7} 2 tracks");
        assert_eq!(rows[1].detail, "SUBRIP \u{b7} Forced");
        assert_eq!(rows[2].detail, "SUBRIP \u{b7} SDH");
    }

    /// The VIDEO and FILE columns, against the design's `streamGroups` for the same item. Each
    /// formatter has one way of being subtly wrong and they are all here: GB is binary, a PMS
    /// bitrate is already kbps, the resolution is the STORED frame and not the `"4k"` class, and
    /// the codec's profile is Title-Cased rather than shouted.
    #[test]
    fn the_file_and_video_columns_reproduce_the_designs_own_content() {
        assert_eq!(fmt_size(34_659_545_780).as_deref(), Some("32.28 GB"));
        assert_eq!(fmt_bitrate(28_852).as_deref(), Some("28.9 Mbps"));
        assert_eq!(fmt_bitrate(24_723).as_deref(), Some("24.7 Mbps"));
        assert_eq!(
            fmt_bitrate(448).as_deref(),
            Some("448 kbps"),
            "below a megabit stays kbps"
        );
        assert_eq!(crate::ui::fmt::clock(9_610_336), "2:40:10");
        assert_eq!(fmt_aspect(2.35).as_deref(), Some("2.35"));
        assert_eq!(
            fmt_codec_profile("hevc", "main 10").as_deref(),
            Some("HEVC (Main 10)")
        );
        assert_eq!(
            fmt_frame_size(3840, 1602).as_deref(),
            Some("3840 \u{d7} 1602")
        );
        assert_eq!(fmt_fps(23.976).as_deref(), Some("23.976 fps"));
        assert_eq!(
            fmt_depth_chroma(10, "4:2:0").as_deref(),
            Some("10-bit \u{b7} 4:2:0")
        );
        // …and the shapes that must NOT draw a row rather than drawing an empty or wrong one
        assert_eq!(fmt_size(0), None);
        assert_eq!(fmt_bitrate(0), None);
        assert_eq!(fmt_fps(0.0), None);
        assert_eq!(fmt_aspect(0.0), None);
        assert_eq!(fmt_depth_chroma(0, ""), None);
        assert_eq!(fmt_frame_size(3840, 0), None);
        assert_eq!(
            fmt_codec_profile("", "main 10"),
            None,
            "no codec is no row, profile or not"
        );
        // a codec with no profile is the bare name, not "HEVC ()"
        assert_eq!(fmt_codec_profile("h264", "").as_deref(), Some("H.264"));
        // 24.000 keeps no decimals; 23.976 keeps all three
        assert_eq!(fmt_fps(24.0).as_deref(), Some("24 fps"));
    }

    /// `5.1(side)` is ffmpeg's channel-ORDER note on an ordinary 5.1 layout, so the parenthetical
    /// is dropped rather than shown — and a file that sends no layout at all still says something
    /// truthful, from the channel count.
    #[test]
    fn a_channel_layout_drops_ffmpegs_order_note_and_falls_back_to_the_count() {
        assert_eq!(fmt_layout("5.1(side)", 6), "5.1");
        assert_eq!(fmt_layout("7.1(wide)", 8), "7.1");
        assert_eq!(fmt_layout("stereo", 2), "Stereo");
        assert_eq!(fmt_layout("mono", 1), "Mono");
        assert_eq!(fmt_layout("", 6), "6 ch");
        assert_eq!(fmt_layout("", 2), "Stereo");
        assert_eq!(
            fmt_layout("", 0),
            "",
            "nothing said at all is nothing shown"
        );
    }

    /// The DOLBY VISION column exists only for a file that has DV, and its five rows are exactly
    /// the design's. Silence must not draw a row: every DV field is 0/false on an ordinary SDR
    /// file, so a column that drew "Profile 0 · Level 0" would appear on the whole library.
    #[test]
    fn the_dolby_vision_column_is_absent_unless_the_file_carries_dolby_vision() {
        let mut d = Detail::default();
        assert!(
            dovi_rows(&d).is_empty(),
            "an SDR file has no third column at all"
        );

        d.dovi = metadata::Dovi {
            present: true,
            profile: 5,
            bl_compat: 0,
            el_present: false,
            level: 6,
            version: (1, 0),
            bl_present: true,
            rpu_present: true,
        };
        let rows = dovi_rows(&d);
        let got: Vec<(&str, &str)> = rows.iter().map(|r| (r.label, r.value.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("Profile", "5"),
                ("Level", "6"),
                ("Version", "1.0"),
                ("Base layer", "Present"),
                ("RPU", "Present"),
            ]
        );

        // present, but the server said nothing else: the column appears with only what it knows
        d.dovi = metadata::Dovi {
            present: true,
            ..metadata::Dovi::NONE
        };
        assert!(
            dovi_rows(&d).is_empty(),
            "DV present with no facts draws no rows"
        );
    }

    /// `DOVIVersion` arrives as the STRING "1.0" and is stored decomposed so `Dovi` can stay
    /// `Copy` (it is read on a per-frame draw path). The round trip has to be exact, and an
    /// unparseable value has to land as "the server said nothing" rather than as a panic or a 0.0.
    #[test]
    fn the_dolby_vision_version_round_trips_through_its_decomposition() {
        use metadata::Dovi;
        assert_eq!(Dovi::parse_version("1.0"), (1, 0));
        assert_eq!(Dovi::parse_version("2.1"), (2, 1));
        assert_eq!(
            Dovi::parse_version("1.0.0"),
            (1, 0),
            "a three-part version keeps two"
        );
        assert_eq!(
            Dovi::parse_version("2"),
            (2, 0),
            "a bare major is major-only"
        );
        assert_eq!(Dovi::parse_version(" 1.0 "), (1, 0));
        for junk in ["", "  ", "abc", "1.x", "-1.0", "1.-2"] {
            assert_eq!(
                Dovi::parse_version(junk),
                (0, 0),
                "{junk:?} must read as unsaid"
            );
        }
        let dv = Dovi {
            version: (1, 0),
            ..Dovi::NONE
        };
        assert_eq!(dv.version_str().as_deref(), Some("1.0"));
        assert_eq!(Dovi::NONE.version_str(), None, "unsaid draws no row");
    }

    /// **The page count is derived, not the mock's hardcoded 3**, and the last page must land
    /// FLUSH on the end of the content — the property that makes the bottom of a list reachable.
    #[test]
    fn paging_covers_the_content_and_the_last_page_lands_on_its_end() {
        let view = 467.0;
        // content that fits is one page and never scrolls
        assert_eq!(pages_for(400.0, view), 1);
        assert_eq!(pages_for(view, view), 1);
        assert_eq!(scroll_for(1, 400.0, view), 0.0);
        assert_eq!(
            scroll_for(9, 400.0, view),
            0.0,
            "a page past the end still shows the top"
        );

        // the real item: 1276px of body in a 467px window
        let content = 1276.0;
        let pages = pages_for(content, view);
        assert_eq!(pages, 4, "809px of overflow needs three 370px steps");
        assert_eq!(scroll_for(1, content, view), 0.0);
        assert_eq!(scroll_for(2, content, view), 370.0);
        assert_eq!(scroll_for(3, content, view), 740.0);
        // the LAST page is clamped to the overflow, not 3*370=1110 — otherwise the tail of the
        // list scrolls off the top and the panel ends on blank space
        assert_eq!(scroll_for(pages, content, view), content - view);
        assert!(scroll_for(pages, content, view) < 3.0 * PAGE_STEP);
        // …and every page is reachable and monotone
        for p in 1..pages {
            assert!(scroll_for(p, content, view) < scroll_for(p + 1, content, view));
        }
    }

    /// The mask's two edges are INDEPENDENT and each states one fact: there is more above, there
    /// is more below. The single-page case wearing neither is the affordance working — a feather
    /// on a body with nothing behind it would say the list continues when it does not.
    #[test]
    fn each_mask_edge_appears_only_when_there_is_content_behind_it() {
        assert_eq!(
            mask_edges(1, 1),
            (false, false),
            "one page: no fade at either end"
        );
        assert_eq!(
            mask_edges(1, 4),
            (false, true),
            "the top page fades only downward"
        );
        assert_eq!(
            mask_edges(2, 4),
            (true, true),
            "a middle page fades both ways"
        );
        assert_eq!(mask_edges(3, 4), (true, true));
        assert_eq!(
            mask_edges(4, 4),
            (true, false),
            "the last page fades only upward"
        );
    }

    /// The feather's ramp, at the geometry it actually runs at. The properties that matter are
    /// that it reaches BOTH ends (a ramp that never gets to 0 leaves a hard cut, one that never
    /// reaches 1 dims the whole body), that it is monotone, and that a disabled edge is exactly
    /// 1.0 — the single-page body must not be dimmed at all.
    #[test]
    fn the_feather_ramps_to_zero_at_an_active_edge_and_is_inert_at_an_idle_one() {
        let (top, bottom) = (100.0f32, 600.0f32);
        let both = (true, true);
        assert_eq!(
            feather_alpha(top, top, bottom, both),
            0.0,
            "flush with the top edge: gone"
        );
        assert_eq!(
            feather_alpha(bottom, top, bottom, both),
            0.0,
            "flush with the bottom: gone"
        );
        assert_eq!(
            feather_alpha(top + FEATHER, top, bottom, both),
            1.0,
            "past the band: full"
        );
        assert_eq!(
            feather_alpha(350.0, top, bottom, both),
            1.0,
            "the middle is untouched"
        );
        // monotone rising across the top band
        let mut last = -1.0;
        for i in 0..=8 {
            let a = feather_alpha(top + FEATHER * i as f32 / 8.0, top, bottom, both);
            assert!(a >= last, "the ramp must not go backwards");
            last = a;
        }
        // an INACTIVE edge does not dim — the first page's top and the last page's bottom
        assert_eq!(feather_alpha(top, top, bottom, (false, true)), 1.0);
        assert_eq!(feather_alpha(bottom, top, bottom, (true, false)), 1.0);
        assert_eq!(feather_alpha(top, top, bottom, (false, false)), 1.0);
        // a body SHORTER than two feathers dissolves from both sides rather than one winning
        let short = feather_alpha(150.0, 100.0, 220.0, both);
        assert!(
            short > 0.0 && short < 1.0,
            "overlapping bands both apply: {short}"
        );
    }

    /// The rail states the same two facts the mask does, in the design's own arithmetic: how far
    /// down you are, and how much of the whole you can see.
    #[test]
    fn the_rail_fill_reports_the_page_and_the_share_of_the_whole() {
        let (t, h) = rail_fill(1, 1);
        assert_eq!(
            (t, h),
            (0.0, 1.0),
            "one page fills the rail — 'this is all of it'"
        );
        for (page, pages, want_t, want_h) in [
            (1, 4, 0.0, 0.25),
            (2, 4, 0.25, 0.25),
            (4, 4, 0.75, 0.25),
            (2, 3, 1.0 / 3.0, 1.0 / 3.0),
        ] {
            let (t, h) = rail_fill(page, pages);
            assert!(
                (t - want_t).abs() < 1e-6,
                "top {t} != {want_t} at {page}/{pages}"
            );
            assert!(
                (h - want_h).abs() < 1e-6,
                "height {h} != {want_h} at {page}/{pages}"
            );
            assert!(
                t + h <= 1.0 + 1e-6,
                "the fill must never run past the track"
            );
        }
        // a nonsense page or count cannot produce a fill outside the track
        let (t, h) = rail_fill(0, 0);
        assert!((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&h));
    }

    /// The panel's frame is centred and clears the glass keep-out on all four sides, which the
    /// design asks of every panel in the set (`--glass-edge-clear` 68). Its body band is the frame
    /// minus the header, the footer and the rail — computed here rather than written down, so the
    /// two can never disagree.
    #[test]
    fn the_panel_is_centred_and_its_body_sits_inside_the_padding() {
        let r = panel_rect();
        // CENTRED, asserted as centring rather than as the mock's literal `left:340px; top:130px`.
        // Those two numbers were the design's placement of an 820-tall sheet; this one is 796 (the
        // 24px trimmed from under the BACK hint — `KeyHint::pad_below`), so a literal y here would
        // have to be re-typed every time the panel's height moves, which is how it would come to
        // pin a stale number instead of the property.
        assert_eq!((r.w, r.h), (PANEL_W, PANEL_H));
        assert_eq!(r.x, (SCR_W - PANEL_W) * 0.5, "centred horizontally");
        assert_eq!(r.y, (SCR_H - PANEL_H) * 0.5, "…and vertically");
        assert_eq!(
            r.x, 340.0,
            "which for this width is still the design's own x"
        );
        assert!(
            r.x >= 68.0 && r.y >= 68.0,
            "clears --glass-edge-clear on the near sides"
        );
        assert!(
            SCR_W - (r.x + r.w) >= 68.0 && SCR_H - (r.y + r.h) >= 68.0,
            "…and the far ones"
        );
        let b = body_rect();
        assert!(b.y > r.y + PAD, "the body starts below the header");
        assert!(
            b.y + b.h < r.y + PANEL_H - PAD,
            "…and ends above the footer"
        );
        assert!(
            b.h > 400.0,
            "the body is the panel's tallest region: {}",
            b.h
        );
        // the rail's gutter is reserved OUT of the body, so a long value can never run under it
        assert!((b.x + b.w + RAIL_GAP + RAIL_W) - (r.x + PANEL_W - PAD) < 0.01);
    }

    /// The Languages column's gate. A SHOW borrows its streams from episode 1, so a panel on a show page
    /// would print another item's path and size under the show's name — `part` is the field that
    /// is empty exactly then.
    #[test]
    fn the_panel_is_offered_only_for_an_item_with_a_file_of_its_own() {
        let _g = crate::testlock::serial();
        metadata::install_for_test(None);
        assert!(!is_available(), "nothing loaded yet: no disc");

        let show = Detail {
            is_show: true,
            part: String::new(),
            ..Default::default()
        };
        metadata::install_for_test(Some(show));
        assert!(
            !is_available(),
            "a show has no file of its own — its streams are episode 1's"
        );

        let movie = Detail {
            part: "/library/parts/751/1745595530/file.mp4".into(),
            ..Default::default()
        };
        metadata::install_for_test(Some(movie));
        assert!(
            is_available(),
            "a leaf has a part, so there is a file to describe"
        );
        metadata::install_for_test(None);
    }
}
