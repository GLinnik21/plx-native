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
const COLUMN_ROWS: usize = 14;

/// The previous sample's fed totals and the tick they were taken at — what turns two totals into
/// a RATE. Without it the panel can only say "1180 AUs have been fed", which stays true and stays
/// large for as long as the app runs, including for the whole of a lane that stopped feeding
/// thirty seconds ago. `(video, audio, at)`; `at == 0` means there is no previous sample yet.
static mut PREV_FED: (i64, i64, u32) = (0, 0, 0);
/// Fed-rate history, per lane, one entry per sample — the CHART, and the only thing on this panel
/// that can answer "when did it stop, and did it come back".
///
/// Every other field is instantaneous, and a class of fault is invisible to all of them: "video
/// plays but there is no sound after scrubbing" is one lane ceasing to advance while the other
/// carries on. The totals stay large, the skew only says how far apart they are NOW, and neither
/// says when it happened or whether it recovered. Thirty-two seconds of history at [`SAMPLE_MS`]
/// does, and a dead lane draws as a flat gap that is unmistakable in a photograph.
const HIST_N: usize = 64;
static mut HIST_V: [u16; HIST_N] = [0; HIST_N];
static mut HIST_A: [u16; HIST_N] = [0; HIST_N];
static mut HIST_HEAD: usize = 0;
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
        let prev = addr_of_mut!(PREV_FED).read();
        addr_of_mut!(HEAD).write(header());
        addr_of_mut!(LEFT).write(left_column(&d, prev, now));
        addr_of_mut!(RIGHT).write(right_column(&d));
        addr_of_mut!(PREV_FED).write((d.fed_v, d.fed_a, now));
        // the same two rates the Fed row prints, kept as a ring so the chart can show their shape
        let (rv, ra) = fed_rates(&d, prev, now);
        let h = addr_of_mut!(HIST_HEAD).read();
        (*addr_of_mut!(HIST_V))[h] = rv.min(u16::MAX as i64) as u16;
        (*addr_of_mut!(HIST_A))[h] = ra.min(u16::MAX as i64) as u16;
        addr_of_mut!(HIST_HEAD).write((h + 1) % HIST_N);
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
fn left_column(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> Vec<Field> {
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
    // Totals AND their rate. A total is monotonic, so it cannot express "this lane stopped" —
    // which is exactly the shape of "video plays but there is no sound after a seek", where the
    // audio lane's total stays large and stops moving. `+0/s` beside a healthy video rate says
    // that in one line; the total alone never can.
    v.push(Field::new("Fed v/a", format!("{} / {}", d.fed_v, d.fed_a)).fault(d.fed_v == 0 && d.load_completed));
    // Its own row: totals plus rate on one line overflowed into the right column's keys.
    v.push(Field::new("Feed rate", fed_rate(d, prev, now)));
    v.push(Field::new("Frames", match (d.frames, d.seen_frame) {
        (0, false) => "0 — none this session".to_string(),
        (0, true) => "0 — since seek".to_string(),
        (n, _) => n.to_string(),
    })
    .fault(!d.seen_frame && d.load_completed));
    let (cv, _ca) = crate::player::aq_caps();
    // One unit for the pair rather than three separate `mb()` runs: "8.0 MB / 8.0 MB · 186 kB"
    // overflowed the value column into the right-hand one.
    v.push(Field::new(
        "Buffered v/a",
        format!("{:.1}/{:.1} MB · {}", d.aq_video as f64 / (1 << 20) as f64, cv as f64 / (1 << 20) as f64, mb(d.aq_audio)),
    ));
    v
}

/// RIGHT — the source and the build. What was asked for, and what the app is made of.
fn right_column(d: &crate::player::Diag) -> Vec<Field> {
    let mut v = Vec::with_capacity(COLUMN_ROWS);
    v.push(Field::section("STREAM"));
    v.push(Field::new("Source", if crate::route::is_transcoding() { "transcode" } else { "direct play" }));
    // SOURCE codec → what the Load payload actually declared. The arrow is the point: this repo's
    // documented silent-audio bug is a payload built from the source rather than from the
    // /decision OUTPUT, and it is invisible in either half on its own.
    v.push(Field::new("Video", chain(crate::route::source_vcodec(), crate::route::stream_vcodec(), d.load_v_str())));
    // …and the same for audio, where `needAudio:false` is a COMPLETE explanation for silence:
    // the pipeline was never asked for any.
    v.push(
        Field::new("Audio", chain(crate::route::source_acodec(), crate::route::stream_acodec(), d.load_a_str()))
            .fault(d.load_a == 0 && d.load_v != 0),
    );
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
    // The two lanes' high-water fed PTS, differenced. Instantaneous and exact where the totals
    // are blind: a skew that keeps growing is the audio lane starving behind the video one, and a
    // skew near zero says both are keeping up and any missing sound is downstream of us.
    v.push(Field::new("A/V skew", skew(d)).fault(skew_bad(d)));
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

/// AUs per second per lane since the previous sample, as ` · +24/+0 /s`. Empty until there IS a
/// previous sample, and empty if the clock went backwards (an SDL tick wrap) rather than printing
/// a negative rate that would read as a fault.
fn fed_rate(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> String {
    let (pv, pa, at) = prev;
    if at == 0 || now <= at {
        return "—".to_string();
    }
    let (rv, ra) = fed_rates(d, prev, now);
    let _ = (pv, pa);
    format!("+{rv} / +{ra} AU/s")
}

/// AUs/second per lane since the previous sample. ONE derivation, shared by the Fed row and the
/// chart, so the number and the bar can never disagree. `(0, 0)` with no previous sample.
fn fed_rates(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> (i64, i64) {
    let (pv, pa, at) = prev;
    if at == 0 || now <= at {
        return (0, 0);
    }
    let dt = (now - at) as f64 / 1000.0;
    (
        ((d.fed_v - pv).max(0) as f64 / dt).round() as i64,
        ((d.fed_a - pa).max(0) as f64 / dt).round() as i64,
    )
}

/// How far the audio lane trails the video lane, in whole seconds of stream time.
fn skew(d: &crate::player::Diag) -> String {
    if d.fed_v_pts == 0 && d.fed_a_pts == 0 {
        return "—".to_string();
    }
    let ms = (d.fed_v_pts - d.fed_a_pts) / 1_000_000;
    format!("{:+.1} s", ms as f64 / 1000.0)
}

/// A lane trailing by more than this is starving rather than merely interleaved. Real containers
/// interleave a fraction of a second either way; whole seconds mean one lane has stopped.
const SKEW_FAULT_MS: i64 = 3_000;

fn skew_bad(d: &crate::player::Diag) -> bool {
    (d.fed_v_pts != 0 || d.fed_a_pts != 0)
        && ((d.fed_v_pts - d.fed_a_pts) / 1_000_000).abs() > SKEW_FAULT_MS
}

/// The codec's whole journey on one line: **what the file is → what the server sends → what we
/// declared to Starfish**.
///
/// Three stages because three different things can be wrong and they are indistinguishable from
/// any one of them. `hevc → h264 → H264` is a server re-encode working correctly. `hevc → hevc →
/// H264` is the payload lying to the decoder, which is this repo's documented silent-audio /
/// stalled-video bug. `hevc → h264 → H265` is the same mistake the other way. The middle stage is
/// collapsed when it equals the source, so a direct play reads `h264 → H264` and only a real
/// server-side transform costs the extra arrow.
fn chain(src: String, sent: String, payload: &str) -> String {
    let src = if src.is_empty() { "—".to_string() } else { src };
    let sent = if sent.is_empty() { "—".to_string() } else { sent };
    if src == sent {
        format!("{src} → {payload}")
    } else {
        format!("{src} → {sent} → {payload}")
    }
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
    // FIXED at the row budget rather than measured from the current rows. Two reasons: the chart
    // lives in whatever the right column does not use, so a height measured from the rows would
    // end above it — which is exactly how the chart first shipped drawing outside the card — and
    // a panel that resizes as rows come and go is a panel that moves under the camera.
    let w = 2.0 * FIELD_COL_W + theme::space::MD + 2.0 * PAD;
    let h = HEAD_H + FieldList::height(COLUMN_ROWS) + PAD;
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
    let rx = inner + FIELD_COL_W + theme::space::MD;
    FieldList::new(unsafe { &*addr_of_mut!(LEFT) }, Rect::new(inner, top, FIELD_COL_W, h)).draw(&e, p);
    FieldList::new(unsafe { &*addr_of_mut!(RIGHT) }, Rect::new(rx, top, FIELD_COL_W, h)).draw(&e, p);

    // The chart, in the right column's own slack. The two columns are deliberately unequal — the
    // stall discriminators all live on the left — so this costs no height at all.
    let used = unsafe { (*addr_of_mut!(RIGHT)).len() };
    let cy = top + FieldList::height(used) + theme::space::XS;
    draw_chart(p, Rect::new(rx, cy, FIELD_COL_W, FieldList::height(COLUMN_ROWS - used) - theme::space::XS));
}

/// The fed-rate chart: one bar per sample, video over audio, sharing a y scale so the two lanes are
/// directly comparable. A lane that stopped draws as a flat gap — which is the whole point, and is
/// the one fault shape no instantaneous field on this panel can express.
///
/// Deliberately LOCAL rather than a `ui::widgets` component. It is a debug read-out, not a design
/// system piece: it owns no tokens beyond a tint, has no focus, no state and no springs, and
/// promoting it would put a diagnostic-only shape in the shared vocabulary for one caller.
fn draw_chart(p: Painter, r: Rect) {
    if r.h < 40.0 {
        return;
    }
    let (hv, ha, head) =
        unsafe { (*addr_of_mut!(HIST_V), *addr_of_mut!(HIST_A), addr_of_mut!(HIST_HEAD).read()) };
    // ONE scale for both lanes, or a dead audio lane would be rescaled back up to look busy.
    let peak = hv.iter().chain(ha.iter()).copied().max().unwrap_or(0).max(1) as f32;

    let lab_h = 22.0;
    if let Ok(cs) = CString::new(format!("FED AU/s — LAST {}s", HIST_N * SAMPLE_MS as usize / 1000)) {
        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .bold()
            .draw(p, Rect::new(r.x, r.y, r.w, lab_h));
    }
    let band = ((r.h - lab_h) * 0.5 - 4.0).max(8.0);
    for (i, (hist, tint)) in [(&hv, theme::TEXT_PRIMARY), (&ha, theme::RESUME_FILL)].into_iter().enumerate() {
        let by = r.y + lab_h + i as f32 * (band + 4.0);
        // the ground, so a run of zeroes reads as a gap in a bar row rather than as blank panel
        p.rect(Rect::new(r.x, by + band - 1.0, r.w, 1.0), 0.0, theme::TEXT_TERTIARY, theme::TEXT_TERTIARY, 0.0);
        let bw = r.w / HIST_N as f32;
        for k in 0..HIST_N {
            // oldest first: `head` is where the NEXT sample lands, so it is also the oldest slot
            let v = hist[(head + k) % HIST_N] as f32 / peak;
            let bh = (v * band).max(0.0);
            if bh < 1.0 {
                continue;
            }
            p.rect(Rect::new(r.x + k as f32 * bw, by + band - bh, (bw - 1.0).max(1.0), bh), 0.0, tint, tint, 0.0);
        }
    }
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
            let l = left_column(&d, (0, 0, 0), 1_000);
            assert!(l.len() <= COLUMN_ROWS, "left at vp={vp}: {}", l.len());
            assert!(right_column(&d).len() <= COLUMN_ROWS, "right at vp={vp}");
        }
    }

    /// A fresh, never-started session must read as faults, not as a healthy zero — that is the
    /// state a user photographs when nothing happens at all, and every row that can say "this did
    /// not happen" must say it.
    #[test]
    fn a_dead_session_marks_its_faults() {
        let d = crate::player::Diag { load_completed: true, ..Default::default() };
        let rows = left_column(&d, (0, 0, 0), 1_000);
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

    /// …and it must leave the MAJORITY of the picture visible, or it is the full-screen card
    /// again and "is anything on screen?" — the question, when playback is broken — stops being
    /// answerable while the read-out is up. 40% is the line rather than a third: the codec rows
    /// and the chart are worth the four points, and a corner panel at 35% still shows most of the
    /// frame. What is NOT negotiable is that it stays a corner panel; if this ever needs raising
    /// again, shrink the type instead.
    #[test]
    fn it_leaves_most_of_the_picture_visible() {
        let a = (2.0 * FIELD_COL_W + theme::space::MD + 2.0 * PAD)
            * (HEAD_H + FieldList::height(COLUMN_ROWS) + PAD);
        let pct = 100.0 * a / (SCR_W * SCR_H);
        assert!(pct < 40.0, "panel covers {pct:.0}% of the screen");
    }

    /// THE case this pair exists for: "video plays but there is no sound after scrubbing". The
    /// audio lane stopped 30 s ago, so its TOTAL is still large — every instantaneous field reads
    /// healthy — and only the rate and the skew can see it.
    #[test]
    fn a_stalled_audio_lane_is_visible_even_though_its_total_is_large() {
        let d = crate::player::Diag {
            load_completed: true,
            fed_v: 5_000,
            fed_a: 4_000,          // large, and unmoved since the previous sample
            fed_v_pts: 60_000_000_000,
            fed_a_pts: 30_000_000_000, // 30 s behind
            ..Default::default()
        };
        let rows = left_column(&d, (4_400, 4_000, 500), 1_000);
        let rate = rows.iter().find(|f| f.key == "Feed rate").expect("Feed rate row");
        assert_eq!(rate.val.as_deref(), Some("+1200 / +0 AU/s"), "the rate must show the dead lane");
        let fed = rows.iter().find(|f| f.key == "Fed v/a").expect("Fed row");
        assert_ne!(fed.tone, crate::ui::widgets::Tone::Fault, "video IS feeding — that row is not the fault");

        let sk = right_column(&d).into_iter().find(|f| f.key == "A/V skew").expect("skew row");
        assert_eq!(sk.val.as_deref(), Some("+30.0 s"));
        assert_eq!(sk.tone, crate::ui::widgets::Tone::Fault, "30 s of skew is a fault");
    }

    /// Ordinary interleave is not a fault — containers put the two lanes a fraction of a second
    /// apart by construction, and flagging that would cry wolf on every healthy playback.
    #[test]
    fn ordinary_interleave_is_not_a_fault() {
        let d = crate::player::Diag {
            fed_v_pts: 10_400_000_000,
            fed_a_pts: 10_000_000_000, // 0.4 s
            ..Default::default()
        };
        assert!(!skew_bad(&d), "0.4 s apart is normal interleave");
        assert_eq!(skew(&d), "+0.4 s");
    }

    /// Before playback there is nothing to compare — the row must say so rather than print a
    /// confident 0.0 s that reads as "both lanes are in step".
    #[test]
    fn skew_is_unknown_before_anything_is_fed() {
        assert_eq!(skew(&crate::player::Diag::default()), "—");
        assert!(!skew_bad(&crate::player::Diag::default()));
    }

    /// No previous sample, and a backwards clock (an SDL tick wrap), must both yield no rate
    /// rather than a negative one that would read as a fault.
    #[test]
    fn a_rate_needs_two_samples_and_a_forward_clock() {
        let d = crate::player::Diag { fed_v: 10, fed_a: 10, ..Default::default() };
        assert_eq!(fed_rate(&d, (0, 0, 0), 1_000), "—", "no previous sample");
        assert_eq!(fed_rate(&d, (0, 0, 2_000), 1_000), "—", "clock went backwards");
    }

    /// The codec chain: a direct play collapses the middle stage, a real server transform shows
    /// all three, and a payload disagreeing with what is being sent is visible as a mismatch
    /// between the last two — which is the whole reason the row has three stages.
    #[test]
    fn the_codec_chain_shows_the_server_transform_only_when_there_is_one() {
        assert_eq!(chain("h264".into(), "h264".into(), "H264"), "h264 → H264");
        assert_eq!(chain("hevc".into(), "h264".into(), "H264"), "hevc → h264 → H264");
        // the bug shape: the server re-encoded to h264 and we told the decoder H265
        assert_eq!(chain("hevc".into(), "h264".into(), "H265"), "hevc → h264 → H265");
        // and nothing known yet must not render as blank gaps around arrows
        assert_eq!(chain(String::new(), String::new(), "—"), "— → —");
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
