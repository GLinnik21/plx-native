//! **Stats for nerds** — the on-screen diagnostics readout, toggled from the player's `…` overflow
//! popover ([`crate::ui::more_menu`]) and from the account menu on Home.
//!
//! # Why this exists, which is not why YouTube's does
//!
//! YouTube ships one so power users can argue about bitrate. This one is a support channel. The app
//! is reviewed and used on televisions nobody here owns — the webOS 6/10 playback failure that
//! prompted it was reported from hardware we cannot buy — and **every other diagnostic surface in
//! this codebase is compiled out of the build a user installs**: the ~40 `/tmp/plxnative-*`
//! triggers, the remote FIFO and the capture stream all sit behind the `devtriggers` feature, which
//! `RELEASE=1` drops. What is left is "ssh in as root and send us `/tmp/plxnative-events.log`",
//! which asks a stranger for shell access to their own television.
//!
//! A panel they can open with the remote and photograph with a phone needs none of that. So this
//! ships in RELEASE builds, and it opens no `/tmp` path, listens on no socket and reads no trigger —
//! it is a product feature, not a debug surface, and it must stay that way.
//!
//! # The photograph is the output format, and it dictates the design
//!
//! Three consequences, all of them load-bearing:
//!
//! **It must be legible in a phone photo of a television across a room.** That is a harder floor
//! than the couch floor: the camera undersamples the panel and chroma-subsamples the result. Values
//! are `size::BODY` over a near-black opaque ground so the contrast is luminance, not hue, and any
//! severity signal is carried by a WORD as well as a colour. `size::MICRO` is banned here (its own
//! token doc bans it for content) — fitting more rows by shrinking them defeats the feature.
//!
//! **It must not become the screen.** It is sized to its CONTENT and parked top-left, covering
//! under a third of the panel and sitting entirely clear of the transport, so playback stays
//! visible around it — the first version was a full-screen opaque card, which made "is anything on
//! screen?" unanswerable at exactly the moment that is the question. It never scrolls, either: a
//! read-out you have to scroll is two photographs and a chance of missing the line that mattered.
//! When a new field will not fit, an existing one has to go.
//!
//! **It must hold still.** Values are sampled at [`SAMPLE_MS`] and held, not read per frame. A
//! number that changes between the viewfinder and the shutter is a number the report cannot be
//! trusted on, and re-rendering ~20 volatile strings a frame would also thrash the whole-string
//! glyph cache, which is a measured failure mode in this repo rather than a worry.
//!
//! # What may never appear on it
//!
//! A photograph cannot be audited, edited, redacted, or scanned by anyone's secret detection — and
//! it lands in a public issue thread that is archived and indexed. So this panel is deliberately a
//! **strict subset** of what the event log records, with tighter rules, and the rules are
//! structural rather than a matter of care:
//!
//! * **No field is ever a URL or a path.** The PMS token rides in the query string of every
//!   playback and image URL, appended at one choke point in `plex::client`, so a URL-shaped field
//!   is a guaranteed credential leak rather than a possible one. Anything URL-derived is decomposed
//!   into non-secret parts (mode, endpoint KIND, throughput) before it can reach a draw call.
//! * **No credential, at any length.** Tokens are omitted, never masked: a PMS token is short,
//!   unstructured and shape-indistinguishable from an ordinary opaque id, so no reader will spot
//!   one and no prefix of it is safe to show.
//! * **No stable identity.** The server's friendly name defaults to the owner's hostname (commonly
//!   their real first name), and its `machineIdentifier` is a permanent household fingerprint that
//!   would link every photograph one person ever posts. Neither is shown, and neither is the
//!   address: nothing about a firmware playback bug depends on what the server is called or where
//!   it sits.
//! * **No viewing identity.** What is playing appears only as its technical shape — dimensions,
//!   position, duration, byte size, direct-play vs transcode. The title, episode title and summary
//!   are omitted: no playback bug depends on them, and they are what turns an anonymous technical
//!   photograph into an attributable one.
//!
//! The enforcement is structural: every row is built by [`left_column`]/[`right_column`] out of a
//! single [`crate::player::Diag`], whose fields are all numbers, booleans and enums — plus the
//! compositor's own `windowId`, which is a bounded `char[64]` assigned by the TV. There is no path
//! by which code elsewhere can push a string onto this panel, so adding a field is a deliberate
//! edit to the one file that carries these rules.
use crate::ui::label::Label;
use crate::ui::widgets::{Field, FieldList, FIELD_COL_W};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::ptr::addr_of_mut;
use std::sync::atomic::{AtomicBool, Ordering};

/// Is the read-out on screen?
///
/// A plain flag, and it takes NO KEYS AT ALL — not a route, not a modal, not even a BACK handler.
/// It is turned off the way it was turned on: by unticking the same checkbox. That is deliberate
/// and it is what keeps the whole feature out of the input path — every transport key keeps working
/// underneath it, which matters, because watching `Fed v/a` and `Frames` move as you press play is
/// how you tell a wedged seek from a wedged load. A BACK handler was tried and removed: it bought
/// one convenience and cost a special case sniffed above every route arm, in a chain where
/// `make lint` cannot see a narrower condition placed after a broader one.
static ON: AtomicBool = AtomicBool::new(false);

pub(crate) fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Flip the readout. Deliberately NOT persisted across launches: the reviewer flow is "open the
/// menu, tick it, reproduce, photograph" inside one session, and a diagnostic overlay that
/// survives a restart is one a user can strand themselves with.
pub(crate) fn toggle() {
    ON.fetch_xor(true, Ordering::Relaxed);
    kick();
}

/// Force it off. The only caller is the leave-playback ritual — a diagnostics panel that survived
/// into the next session would be a bug in the feature built to find bugs.
pub(crate) fn close() {
    ON.store(false, Ordering::Relaxed);
    kick();
}

fn kick() {
    // Discrete change: the whole-frame present gate has no spring to watch here, so without this
    // the panel would not appear until something else happened to repaint. See `ui::idle`.
    unsafe { addr_of_mut!(NEXT_SAMPLE).write(0) };
    crate::ui::idle::invalidate();
}

// ---- the snapshot -----------------------------------------------------------------------------

/// How often the values are re-read and re-formatted. **Not a render rate** — the panel draws every
/// frame from this snapshot.
///
/// Two reasons it is not per-frame, and they point the same way. The glyph cache holds 160
/// whole-string slots and this panel carries ~20 volatile values; re-formatting them each frame
/// thrashes it, which is a measured cost in this repo rather than a worry. And a number that
/// changes between the viewfinder and the shutter is a number the report cannot be trusted on.
const SAMPLE_MS: u32 = 500;
/// Rows per column. The budget, not a preference: the panel is sized to exactly this and never
/// scrolls — a read-out you have to scroll is two photographs and a chance of missing the line
/// that mattered. A new field costs an existing one.
const COLUMN_ROWS: usize = 12;

static mut NEXT_SAMPLE: u32 = 0;
static mut LEFT: Vec<Field> = Vec::new();
static mut RIGHT: Vec<Field> = Vec::new();
static mut HEAD: [String; 3] = [String::new(), String::new(), String::new()];

/// Re-sample if the hold has expired. Main-thread only (it is called from the frame loop).
pub(crate) fn update(now: u32) {
    if !enabled() {
        return;
    }
    let due = unsafe { addr_of_mut!(NEXT_SAMPLE).read() };
    if now < due {
        return;
    }
    unsafe {
        addr_of_mut!(NEXT_SAMPLE).write(now.wrapping_add(SAMPLE_MS));
        // ONE sample feeding both columns. Calling `diag()` per column would let the left one
        // report "no frames" beside a right-hand position taken a moment later — a panel that
        // tells a story that never happened is worse than no panel.
        let d = crate::player::diag();
        addr_of_mut!(HEAD).write(header());
        addr_of_mut!(LEFT).write(left_column(&d));
        addr_of_mut!(RIGHT).write(right_column(&d));
    }
    // a re-sample changes what is on screen, and no spring is involved — see `ui::idle`
    crate::ui::idle::invalidate();
}

fn header() -> [String; 3] {
    let w = crate::webos::info();
    let os = if w.major == 0 {
        "webOS unknown — /var/run/nyx/os_info.json unreadable".to_string()
    } else {
        format!("webOS {} · {} · api {}", w.release, w.codename, w.api)
    };
    let (_, _, vw, vh) = crate::surface::viewport();
    [
        format!("PlxNative {}", env!("CARGO_PKG_VERSION")),
        format!("{os} · surface {vw}x{vh}"),
        playback_line(),
    ]
}

/// The one-line verdict, in the largest type on the panel: what the pipeline thinks it is doing.
fn playback_line() -> String {
    use crate::player::PlaybackState as S;
    let s = match crate::player::state() {
        S::Idle => "Idle",
        S::Resolving => "Resolving",
        S::Connecting => "Connecting",
        S::Buffering => "Buffering",
        S::Seeking => "Seeking",
        S::Playing => "Playing",
        S::Error => "Playback error",
    };
    if crate::player::TX.paused.load(Ordering::Relaxed) {
        format!("{s} (paused)")
    } else {
        s.to_string()
    }
}

/// LEFT — the stall discriminators, in the order a maintainer reads them. Everything here answers
/// "how far did this playback get before it stopped?", top to bottom.
fn left_column(d: &crate::player::Diag) -> Vec<Field> {
    let mut v = Vec::with_capacity(COLUMN_ROWS);
    v.push(Field::section("VIDEO PLANE"));
    v.push(Field::new("Mode", d.vp_mode_str()).fault(d.vp_mode == crate::player::VP_NONE));
    if d.vp_mode == crate::player::VP_EXPORTED {
        // webOS 5+: no window means the decoded frames have nowhere to land, and the symptom is
        // sound over a black screen — or, if the payload never got the id, no frames at all.
        v.push(Field::new(
            "Window",
            if d.window_id.is_empty() { "NONE — cannot bind".into() } else { d.window_id.clone() },
        )
        .fault(d.window_id.is_empty()));
        v.push(Field::new("Placed", match d.place_rv {
            i32::MIN => "not placed".to_string(),
            rv => format!("{}x{} rv={rv}", d.placed_w, d.placed_h),
        })
        .fault(d.place_rv == i32::MIN));
    } else {
        v.push(Field::new("ACB", if d.acb_ok { "bound" } else { "not bound" }).fault(!d.acb_ok));
    }
    v.push(Field::section("PIPELINE"));
    v.push(Field::new("Stage", d.stage_str()));
    v.push(Field::new(
        "Load",
        if d.load_failed { "REFUSED" } else if d.load_completed { "completed" } else { "waiting" },
    )
    .fault(d.load_failed));
    // A completed Load with zero callbacks is the pipeline never speaking to us at all — the
    // sharpest single symptom a stuck-buffering report can carry.
    v.push(Field::new("Callbacks", match d.cb_count {
        0 => "none".to_string(),
        n => format!("{n} · last type {}", d.cb_last),
    })
    .fault(d.cb_count == 0 && d.load_completed));
    v.push(Field::new("Demuxed", if d.pushed_any { "yes" } else { "NOTHING" }).fault(!d.pushed_any));
    v.push(Field::new("Fed v/a", format!("{} / {}", d.fed_v, d.fed_a)).fault(d.fed_v == 0 && d.load_completed));
    v.push(Field::new("Frames", match (d.frames, d.seen_frame) {
        (0, false) => "0 — none this session".to_string(),
        (0, true) => "0 — since seek".to_string(),
        (n, _) => n.to_string(),
    })
    .fault(!d.seen_frame && d.load_completed));
    let (cv, _ca) = crate::player::aq_caps();
    v.push(Field::new("Buffered v/a", format!("{} / {} · {}", mb(d.aq_video), mb(cv), mb(d.aq_audio))));
    v
}

/// RIGHT — the source and the build. What was asked for, and what the app is made of.
fn right_column(d: &crate::player::Diag) -> Vec<Field> {
    let mut v = Vec::with_capacity(COLUMN_ROWS);
    v.push(Field::section("STREAM"));
    v.push(Field::new("Source", if crate::route::is_transcoding() { "transcode" } else { "direct play" }));
    v.push(Field::new("Frame", match (d.video_w, d.video_h) {
        (0, _) | (_, 0) => "unknown — stream never opened".to_string(),
        (w, h) => format!("{w}x{h}"),
    })
    .fault(d.video_w == 0));
    v.push(Field::new(
        "Position",
        format!(
            "{} / {}",
            crate::ui::fmt::clock(d.pos_ns / 1_000_000),
            if d.dur_ns > 0 { crate::ui::fmt::clock(d.dur_ns / 1_000_000) } else { "unknown".into() }
        ),
    ));
    v.push(Field::new("Size", if d.file_size > 0 { mb(d.file_size) } else { "unknown".into() }));
    v.push(Field::section("BUILD"));
    let (fmtv, codv, utlv) = crate::ff::majors();
    v.push(Field::new(
        "FFmpeg",
        if fmtv == 0 { "NOT BOUND".to_string() } else { format!("fmt {fmtv} · cod {codv} · util {utlv}") },
    )
    .fault(fmtv == 0));
    v.push(Field::new("Config", if cfg!(feature = "devtriggers") { "dev" } else { "release" }));
    v
}

/// Bytes as the read-out spells them. Local rather than in `ui::fmt` because it is the only user;
/// promote it there the moment a second screen wants it.
fn mb(b: i64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.2} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} kB", b >> 10),
        b => format!("{b} B"),
    }
}

// ---- drawing ----------------------------------------------------------------------------------

const MARGIN: f32 = 60.0;
const PAD: f32 = 28.0;
/// Height of the header block. It is a SUM, not a guess: title (16 + 40) + two caption lines
/// (2 x 28) + the verdict (32) + air. It was 122 once and the verdict line drew straight through
/// the first section heading — the columns start at exactly this offset, so it has to clear the
/// last thing the header draws, not the last thing it is nominally made of.
const HEAD_H: f32 = 152.0;

/// The panel's box, SIZED TO ITS CONTENT rather than to the screen.
///
/// Two consequences, and both REMOVE code rather than adding it. The video stays visible around it,
/// which is the point of a stats overlay you watch playback under — the first version was a
/// full-screen opaque card that made "is anything on screen?" unanswerable while the panel was up.
/// And it sits entirely ABOVE the transport (`player_hud::CTRL_Y`), so a pointer click can never
/// land on the scrubber's rects THROUGH an opaque card — which was the only reason the click path
/// needed a close-on-click arm at all.
fn panel_rect() -> Rect {
    let rows = unsafe { (*addr_of_mut!(LEFT)).len().max((*addr_of_mut!(RIGHT)).len()) };
    let w = 2.0 * FIELD_COL_W + theme::space::MD + 2.0 * PAD;
    let h = HEAD_H + FieldList::height(rows) + PAD;
    Rect::new(MARGIN, MARGIN, w, h)
}

pub(crate) fn draw() {
    if !enabled() {
        return;
    }
    let p = Painter::root();
    let e = Env::inert();
    let frame = panel_rect();
    // Its own opaque ground. On the player route the UI plane is cleared fully TRANSPARENT, so a
    // scrim would leave the picture showing through the text — the one condition a photograph of
    // this has to survive.
    p.rect(frame, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);

    let head = unsafe { &*addr_of_mut!(HEAD) };
    let inner = frame.x + PAD;
    let iw = frame.w - 2.0 * PAD;
    if let Ok(cs) = CString::new("Diagnostics") {
        Label::new(cs.as_ptr(), theme::size::HEADLINE, theme::TEXT_PRIMARY)
            .bold()
            .draw(p, Rect::new(inner, frame.y + theme::space::SM, iw, 40.0));
    }
    for (i, (line, ink)) in
        [(&head[0], theme::TEXT_TERTIARY), (&head[1], theme::TEXT_SECONDARY)].into_iter().enumerate()
    {
        if let Ok(cs) = CString::new(line.as_str()) {
            Label::new(cs.as_ptr(), theme::size::CAPTION, ink)
                .draw(p, Rect::new(inner, frame.y + 56.0 + i as f32 * 28.0, iw, 28.0));
        }
    }
    // the verdict — the one line that says what the pipeline thinks it is doing
    if let Ok(cs) = CString::new(head[2].as_str()) {
        let ink = if head[2].starts_with("Playback error") { theme::DANGER } else { theme::TEXT_PRIMARY };
        Label::new(cs.as_ptr(), theme::size::BODY, ink)
            .bold()
            .draw(p, Rect::new(inner, frame.y + 112.0, iw, 32.0));
    }

    let top = frame.y + HEAD_H;
    let h = FieldList::height(COLUMN_ROWS);
    FieldList::new(unsafe { &*addr_of_mut!(LEFT) }, Rect::new(inner, top, FIELD_COL_W, h)).draw(&e, p);
    FieldList::new(
        unsafe { &*addr_of_mut!(RIGHT) },
        Rect::new(inner + FIELD_COL_W + theme::space::MD, top, FIELD_COL_W, h),
    )
    .draw(&e, p);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::{SCR_H, SCR_W};

    /// The budget is the design (see [`COLUMN_ROWS`]) — a column that outgrows it does not scroll,
    /// it draws off the bottom of a panel someone is about to photograph. `_serial` because both
    /// builders read crate-global playback state.
    #[test]
    fn neither_column_outgrows_the_page() {
        // Both video-plane shapes: the exported path carries two extra rows that ACB does not, so
        // the wider one is the one that has to fit.
        for vp in [crate::player::VP_ACB, crate::player::VP_EXPORTED, crate::player::VP_NONE] {
            let d = crate::player::Diag { vp_mode: vp, ..Default::default() };
            assert!(left_column(&d).len() <= COLUMN_ROWS, "left at vp={vp}: {}", left_column(&d).len());
            assert!(right_column(&d).len() <= COLUMN_ROWS, "right at vp={vp}");
        }
    }

    /// A fresh, never-started session must read as faults, not as a healthy zero — that is the
    /// state a user photographs when nothing happens at all, and every row that can say "this did
    /// not happen" must say it.
    #[test]
    fn a_dead_session_marks_its_faults() {
        let d = crate::player::Diag { load_completed: true, ..Default::default() };
        let rows = left_column(&d);
        let faults: Vec<_> = rows
            .iter()
            .filter(|f| f.tone == crate::ui::widgets::Tone::Fault)
            .map(|f| f.key)
            .collect();
        for expect in ["Mode", "Callbacks", "Demuxed", "Fed v/a", "Frames"] {
            assert!(faults.contains(&expect), "{expect} should read as a fault; got {faults:?}");
        }
    }

    /// The panel must sit entirely ABOVE the transport's control row. That is what makes a pointer
    /// click unambiguous — no part of the scrubber or the discs is ever underneath an opaque card —
    /// and it is why the click path needs no close-on-click arm.
    #[test]
    fn the_panel_clears_the_transport() {
        let bottom = MARGIN + HEAD_H + FieldList::height(COLUMN_ROWS) + PAD;
        assert!(
            bottom < crate::ui::player_hud::CTRL_Y,
            "panel bottom {bottom} overlaps the control row at {}",
            crate::ui::player_hud::CTRL_Y
        );
        let right = MARGIN + 2.0 * FIELD_COL_W + theme::space::MD + 2.0 * PAD;
        assert!(right < SCR_W, "panel is wider than the screen: {right}");
    }

    /// …and it must leave a real amount of picture visible, or it is the full-screen card again.
    /// A third of the screen is the line: enough room for the read-out, little enough that "is
    /// anything on the panel?" is still answerable while it is up.
    #[test]
    fn it_covers_no_more_than_a_third_of_the_screen() {
        let a = (2.0 * FIELD_COL_W + theme::space::MD + 2.0 * PAD)
            * (HEAD_H + FieldList::height(COLUMN_ROWS) + PAD);
        assert!(a < SCR_W * SCR_H / 3.0, "panel covers {:.0}% of the screen", 100.0 * a / (SCR_W * SCR_H));
    }

    #[test]
    fn bytes_read_the_way_a_human_says_them() {
        assert_eq!(mb(0), "0 B");
        assert_eq!(mb(2048), "2 kB");
        assert_eq!(mb(8 * 1024 * 1024), "8.0 MB");
        assert_eq!(mb(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    /// An unreadable `os_info.json` must say so rather than print a plausible version — the panel
    /// exists to identify firmware, so a confident wrong answer is worse than none. (The parse
    /// itself is pinned in `webos.rs`; this pins the SENTENCE the panel prints.)
    #[test]
    fn an_unknown_firmware_is_named_as_unknown() {
        let _g = crate::testlock::serial();
        let line = &header()[1];
        if crate::webos::info().major == 0 {
            assert!(line.contains("unknown"), "{line}");
        } else {
            assert!(line.starts_with("webOS "), "{line}");
        }
    }
}
