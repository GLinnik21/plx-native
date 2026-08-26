//! **Stats for nerds** — the on-screen diagnostics read-out, toggled from the player's `…` overflow
//! popover ([`crate::ui::more_menu`]).
//!
//! PLAYER-ONLY today: `app.rs` draws it inside the player branch, so a toggle offered anywhere else
//! would tick a box and show nothing. A Home entry point (for "starts but finds no server", which
//! never reaches a player) is a real gap and a separate change — do not advertise it here until the
//! draw call exists.
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
//! compositor's own `windowId`, which is a bounded `char[64]` assigned by the TV, and ONE
//! server-derived string: the PMS release number on the Server row, which identifies a software
//! RELEASE shared by every install of it, not a household (the same reasoning that lets the
//! header print the app's own version). There is no path by which code elsewhere can push a
//! string onto this panel, so adding a field is a deliberate edit to the one file that carries
//! these rules.
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
/// menu, turn it on, reproduce, photograph" inside one session, and a diagnostic overlay that
/// survives a restart is one a user can strand themselves with.
pub(crate) fn toggle() {
    ON.fetch_xor(true, Ordering::Relaxed);
    kick();
}

/// Force it on for an automated playback run. Unlike [`toggle`], this is idempotent so an already
/// visible read-out keeps its sample cadence and does not reset the panel's clock.
pub(crate) fn open() {
    if !ON.swap(true, Ordering::Relaxed) {
        kick();
    }
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
/// Rows in the read-out. The budget, not a preference: the panel is sized to exactly this and never
/// scrolls — a read-out you have to scroll is two photographs and a chance of missing the line
/// that mattered. A new field costs an existing one.
///
/// It was TWO columns of 14 until 2026-08-26, and the arrangement had outgrown the space it was
/// given: the last rows drew below the card, on top of the transport. One column of composed lines
/// carries the same evidence in about three quarters of the area, and a row that answers one
/// question with three facts (`Load completed · 8 callbacks · HTTP 200 · 576 kB`) reads faster
/// across a room than three rows that each answer a third of it.
const PANEL_ROWS: usize = 13;

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
// 32 samples, not 64, and the photograph is why both ways: it halves the chart's per-frame draw
// calls (each bar is its own `p.rect`, on the one route the idle gate deliberately excludes) AND
// it doubles the bar width, which is what makes the shape readable across a room. Sixteen seconds
// still shows a lane dying and recovering.
const HIST_N: usize = 32;
static mut HIST_V: [u16; HIST_N] = [0; HIST_N];
static mut HIST_A: [u16; HIST_N] = [0; HIST_N];
static mut HIST_HEAD: usize = 0;
static mut NEXT_SAMPLE: u32 = 0;
static mut ROWS: Vec<Field> = Vec::new();
static mut HEAD: [String; 2] = [String::new(), String::new()];

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
        // ONE sample feeding the whole panel. Calling `diag()` per block would let one row report
        // "no frames" beside a position taken a moment later — a panel that tells a story that
        // never happened is worse than no panel.
        let d = crate::player::diag();
        let prev = addr_of_mut!(PREV_FED).read();
        // `.replace()`, NOT `.write()`. `<*mut T>::write` is `ptr::write` — it overwrites without
        // DROPPING what was there, and both own heap: a `Vec<Field>` and three `String`s plus
        // every row's value. At 2 Hz that orphaned ~1.4 KB and ~23 allocations every sample, on a
        // panel explicitly designed to be left up for the length of a film.
        drop(addr_of_mut!(HEAD).replace(header(&d, now)));
        drop(addr_of_mut!(ROWS).replace(rows(&d, prev, now)));
        addr_of_mut!(PREV_FED).write((d.fed_v, d.fed_a, now));
        // the same two rates the Fed row prints, kept as a ring so the chart can show their shape
        let (rv, ra) = fed_rates(&d, prev, now).unwrap_or((0, 0));
        let h = addr_of_mut!(HIST_HEAD).read();
        (*addr_of_mut!(HIST_V))[h] = rv.min(u16::MAX as i64) as u16;
        (*addr_of_mut!(HIST_A))[h] = ra.min(u16::MAX as i64) as u16;
        addr_of_mut!(HIST_HEAD).write((h + 1) % HIST_N);
    }
    // a re-sample changes what is on screen, and no spring is involved — see `ui::idle`
    crate::ui::idle::invalidate();
}

/// The two head lines: **who this build is** and **what the pipeline thinks it is doing**.
///
/// It was THREE — build, firmware, verdict — and the draw only ever emits two, which is how the
/// verdict came to be missing from the panel entirely on 2026-08-26: the firmware line took the
/// verdict's slot and drew in its bold face, so a photograph of a FAILED playback showed the
/// firmware where the failure reason should have been. Two lines produced and two drawn, so the
/// array's length is the contract rather than a comment.
fn header(d: &crate::player::Diag, now: u32) -> [String; 2] {
    let w = crate::webos::info();
    let os = if w.major == 0 {
        "webOS unknown — os_info.json unreadable".to_string()
    } else {
        format!("webOS {} · api {}", w.release, w.api)
    };
    let (_, _, vw, vh) = crate::surface::viewport();
    [
        // via `plex::identity`, not a literal + `env!`: that module exists precisely so the
        // product name and version cannot disagree between surfaces, and this one is photographed.
        // The firmware CODENAME is dropped — it identifies the release no better than the release
        // number does, and this line has to fit beside it.
        format!(
            "{} {} · {} · {os} · surface {vw}x{vh}",
            crate::plex::identity::PRODUCT,
            crate::plex::identity::VERSION,
            if cfg!(feature = "devtriggers") { "dev" } else { "release" }
        ),
        playback_line(d, now),
    ]
}

/// The one-line verdict, in the largest type on the panel: what the pipeline thinks it is doing.
fn playback_line(d: &crate::player::Diag, now: u32) -> String {
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
    // The reason is the verdict when there is one — "Playback error" made the reviewer derive
    // "the server dropped the video track" from the server's own transcoder logs (issue #22);
    // this line is the photograph that should have said it.
    if matches!(crate::player::state(), S::Error) {
        return match crate::player::error_reason() {
            "" => s.to_string(),
            why => format!("{s} — {why}"),
        };
    }
    if crate::player::TX.paused.load(Ordering::Relaxed) {
        // the frozen clock must DISARM while paused — a paused picture is not a stalled one
        return format!("{s} (paused)");
    }
    // A stream that says "Playing" while nothing has moved for seconds is the failure with no
    // error at all: the app freezes on its last frame and every other row still reads healthy.
    let stuck = since(d.frame_at, now) / 1000;
    if matches!(crate::player::state(), S::Playing) && d.seen_frame && stuck >= STALL_MS / 1000 {
        return format!("{s} (stalled {stuck} s)");
    }
    s.to_string()
}

/// The whole read-out, in the order a maintainer reads it: **what the model decided, then what the
/// pipeline did with it.** One column of composed lines.
///
/// The ordering rule is worth keeping. The top block is Auto's own state — the panel's most common
/// question by far is "why is the picture this quality", and that is answered by the controller's
/// inputs, not by a callback count. The bottom block is the stall discriminators, unchanged in
/// content: every fault predicate the two-column version carried is still here, OR-ed onto the row
/// that now states its facts.
fn rows(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> Vec<Field> {
    let mut v = Vec::with_capacity(PANEL_ROWS);

    // ---- what is playing -----------------------------------------------------------------
    // What the server IS: release + Plex Pass — issue #22's blind spot (docs/plex-pass-audit.md).
    // "Server sent audio only" is a different report on a server that CANNOT encode video than on
    // one that should have. Never a fault tone: a free server is a fact, not an error.
    // (Name/address/machineIdentifier stay banned; a release number identifies nothing.)
    v.push(Field::new(
        "Source",
        format!(
            "{} · {}",
            match (crate::route::is_transcoding(), crate::route::is_remux()) {
                (false, _) => "direct play",
                (true, true) => "remux (copy)",
                (true, false) => "transcode (re-encode)",
            },
            server_line(&crate::plex::serverinfo::version(), crate::plex::serverinfo::subscription()),
        ),
    ));
    // SOURCE codec → what the Load payload actually declared. The arrow is the point: this repo's
    // documented silent-audio bug is a payload built from the source rather than from the
    // /decision OUTPUT, and it is invisible in either half on its own.
    v.push(Field::new(
        "Video",
        chain(crate::route::source_vcodec(), crate::route::stream_vcodec(), d.load_v_str()),
    ));
    // …and the same for audio, where `needAudio:false` is a COMPLETE explanation for silence:
    // the pipeline was never asked for any.
    v.push(
        Field::new("Audio", chain(crate::route::source_acodec(), crate::route::stream_acodec(), d.load_a_str()))
            .fault(d.load_a == 0 && d.load_v != 0),
    );
    // Raster, position and the two lanes' fed-PTS difference on one line — three facts about the
    // same stream. A skew that keeps growing is the audio lane starving behind the video one; a
    // skew near zero says both keep up and any missing sound is downstream of us.
    v.push(
        Field::new(
            "Frame",
            format!(
                "{} · {} / {} · A/V {}",
                match (d.video_w, d.video_h) {
                    (0, _) | (_, 0) => "stream never opened".to_string(),
                    (w, h) => format!("{w}x{h}"),
                },
                crate::ui::fmt::clock(d.pos_ns / 1_000_000),
                if d.dur_ns > 0 { crate::ui::fmt::clock(d.dur_ns / 1_000_000) } else { "?".into() },
                skew(d),
            ),
        )
        .fault(d.video_w == 0 || skew_bad(d)),
    );

    // ---- the controller ------------------------------------------------------------------
    abr_rows(d, &mut v);

    // ---- how far the pipeline got --------------------------------------------------------
    // The video plane, whichever seam this firmware uses. On webOS 5+ no window means decoded
    // frames have nowhere to land and no placement means the window was never given geometry;
    // both present as "buffering forever". On webOS 4 `acb_bind` returns void, so "bind sent" —
    // the panel must not claim a confirmation the API does not give.
    let (plane, plane_bad) = plane_line(d);
    v.push(Field::new("Plane", plane).fault(plane_bad));
    // Load, its callbacks and the transport on one line. A completed Load with zero callbacks is
    // the pipeline never speaking to us; a LATCHED error is it speaking once and refusing — which
    // read as a perfectly healthy panel before there was a row for it. The HTTP half splits "the
    // bytes never arrived" from "bytes arrived and nothing parsed".
    v.push(
        Field::new(
            "Pipeline",
            format!(
                "Load {} · {} · {}",
                if d.load_failed {
                    "REFUSED"
                } else if d.load_completed {
                    "completed"
                } else {
                    "waiting"
                },
                match (d.cb_count, d.cb_err) {
                    (0, _) => "no callbacks".to_string(),
                    (n, 0) => format!("{n} callbacks"),
                    (n, e) => format!("{n} cb · ERR {e} @ {}", d.cb_err_at),
                },
                match (d.http_status, d.net_rx) {
                    (0, _) => "no connection".to_string(),
                    (st, rx) => format!("HTTP {st} · {}", mb(rx)),
                },
            ),
        )
        .fault(
            d.load_failed
                || d.cb_err != 0
                || (d.cb_count == 0 && d.load_completed)
                || (d.http_status != 0 && !(200..300).contains(&d.http_status)),
        ),
    );
    // The demux → feed lane, end to end. `NOTHING demuxed` is its own sentence rather than a
    // separate row, because a stopped producer and a stopped consumer are read together.
    //
    // The fed AU TOTALS are deliberately not here. They are the one pair on the old panel that
    // said nothing a neighbouring row does not say better: whether anything ever fed is
    // `NOTHING demuxed` (and this row's fault tint), and whether the two lanes are in step is the
    // Frame row's A/V skew, in seconds rather than in counts you have to subtract yourself.
    let (cv, ca) = crate::player::aq_caps();
    v.push(
        Field::new(
            "Feed",
            format!(
                "{} · queue {:.1}/{:.1} · {:.2}/{:.1} MB",
                if d.pushed_any { fed_rate(d, prev, now) } else { "NOTHING demuxed".to_string() },
                mb_f(d.aq_video),
                mb_f(cv),
                mb_f(d.aq_audio),
                mb_f(ca),
            ),
        )
        .fault(!d.pushed_any || (d.fed_v == 0 && d.load_completed) || d.feed_is_fault()),
    );
    // CARRIES THE CLOCK. A photograph has no time axis: "Load completed, 0 frames" is innocent at
    // two seconds and damning at four minutes, and only this row can tell them apart. The feed
    // state rides along because it is the sentence that says WHY the count is not moving.
    v.push(
        Field::new("Frames", format!("{} · {}", frames_str(d, now), d.feed_state_str()))
            .fault(!d.seen_frame && d.load_completed && since(d.load_at, now) > STALL_MS),
    );
    v
}

/// The video plane's whole state as one sentence, and whether it is a fault. Split out because the
/// two seams answer with different facts and the row must not grow a branch per firmware.
fn plane_line(d: &crate::player::Diag) -> (String, bool) {
    // The firmware family is dropped from the two healthy labels — `vp_mode_str`'s
    // "(webOS 4)" / "(webOS 5+)" restates what the header's own `webOS 4.10.2` already says, and
    // it is 11 characters this row does not have. The FAULT arm keeps its full sentence.
    let mode = match d.vp_mode {
        crate::player::VP_EXPORTED => "exported window",
        crate::player::VP_ACB => "ACB",
        _ => d.vp_mode_str(),
    };
    match d.vp_mode {
        crate::player::VP_EXPORTED => {
            let win = if d.window_id.is_empty() { "NO WINDOW".to_string() } else { d.window_id.clone() };
            // `rv == 0` is "the seam had no window or no symbol", NOT "SDL refused" — worded so a
            // reader is not sent looking for a rejection that never happened.
            let placed = match d.place_rv {
                i32::MIN => "not placed".to_string(),
                0 => "PLACE FAILED (rv=0)".to_string(),
                rv => format!("src {}x{} rv={rv}", d.placed_w, d.placed_h),
            };
            (
                format!("{mode} · {win} · {placed}"),
                d.window_id.is_empty() || d.place_rv == i32::MIN || d.place_rv == 0,
            )
        }
        crate::player::VP_ACB => (
            format!("{mode} · {}", match (d.acb_ok, d.stage) {
                (false, _) => "NOT AVAILABLE",
                (true, 0) => "init'd · NOT bound",
                (true, 1) => "bind sent",
                _ => "streaming",
            }),
            !d.acb_ok,
        ),
        // `VP_NONE` and anything unrecognised: there is no video path at all, which is always a
        // fault and is the first row a reader should reach on a set that shows no picture.
        crate::player::VP_NONE | _ => (mode.to_string(), true),
    }
}

/// Milliseconds since an SDL-tick stamp, 0 when never stamped or the clock wrapped.
fn since(at: u32, now: u32) -> u32 {
    if at == 0 || now < at {
        0
    } else {
        now - at
    }
}

/// How long a still panel has to stay still before it is a stall rather than a slow server.
const STALL_MS: u32 = 8_000;

/// The frame count with its clock. `frames` is SEEK-scoped, so "0" has three meanings and the
/// panel has to say which.
fn frames_str(d: &crate::player::Diag, now: u32) -> String {
    // Paused, the frame count SHOULD stop moving. Reporting that as "frozen" sends a reader after
    // a fault that is just the pause button — the verdict line already disarms its own stall clock
    // for exactly this reason, and this row has to agree with it or the panel contradicts itself.
    if crate::player::TX.paused.load(Ordering::Relaxed) {
        return match d.frames {
            0 if !d.seen_frame => "none yet".to_string(),
            n => n.to_string(),
        };
    }
    let stuck = since(d.frame_at.max(d.load_at), now) / 1000;
    match (d.frames, d.seen_frame) {
        (0, false) if d.load_completed => format!("none in {stuck} s"),
        (0, false) => "none yet".to_string(),
        (0, true) => "0 — since seek".to_string(),
        (n, _) if stuck >= STALL_MS / 1000 => format!("{n} · frozen {stuck} s"),
        (n, _) => n.to_string(),
    }
}

/// **Auto's own state — the model, not a summary of it.** Five rows that are, in order, the four
/// inputs the controller actually decides on and then the decision: the operating point, the
/// delivery estimate WITH its uncertainty, the buffer with its slope and starvation horizon, the
/// PMS production constraint kept separate from the network one, and the utility verdict with its
/// reason code.
///
/// It is deliberately not the YouTube naming any more. Those names ("Connection Speed", "Network
/// Activity") describe a client that picks a rendition off a ladder by throughput, which is not
/// what this app does — and a read-out that borrows them cannot say the two things this controller
/// is FOR: that delivery and production are separate constraints, and that an estimate carries its
/// own confidence. A panel photographed to explain a decision has to name the decision's terms.
///
/// A non-Auto playback has no model, so the block collapses to the FFmpeg binding — the one fact
/// that is worth a row when nothing is adapting, and a fault when the bundled libraries did not
/// load at all.
fn abr_rows(d: &crate::player::Diag, v: &mut Vec<Field>) {
    if d.abr_mode == 0 {
        let (fmtv, codv, utlv) = crate::ff::majors();
        v.push(
            Field::new(
                "FFmpeg",
                if fmtv == 0 {
                    "NOT BOUND".to_string()
                } else {
                    format!("fmt {fmtv} · cod {codv} · util {utlv}")
                },
            )
            .fault(fmtv == 0),
        );
        return;
    }
    v.push(Field::new("Quality", abr_quality(d)));
    v.push(Field::new("Link", abr_link(d)));
    v.push(Field::new("Buffer", abr_buffer(d)).fault(d.abr_starve_secs >= 0));
    v.push(Field::new("Server load", abr_server_load(d)));
    v.push(Field::new("Decision", abr_decision(d)));
}

/// The delivery estimate as the model holds it: what selection may SPEND, what was measured, how
/// much the estimate distrusts itself, and how many observations are behind it.
///
/// The safe budget leads because it is the number the choice is actually made with — it is the
/// measured rate already discounted for uncertainty, for a server behind real time and for a
/// reserve below the floor, so a reader who sees `safe 4.0 Mbps` beside `measured 21.4 Mbps` is
/// looking straight at the reason a fast link is not being spent.
fn abr_link(d: &crate::player::Diag) -> String {
    if d.abr_net_kbps < 0 {
        return "waiting for first measurement".to_string();
    }
    let mut s = if d.abr_safe_kbps >= 0 {
        format!("safe {} · measured {}", abr_rate(d.abr_safe_kbps), abr_rate(d.abr_net_kbps))
    } else {
        format!("measured {}", abr_rate(d.abr_net_kbps))
    };
    if d.abr_unc_pm >= 0 {
        // Per-mille in the model, per-cent on the panel: nobody reads a photograph in per-mille.
        s.push_str(&format!(" ±{}%", d.abr_unc_pm / 10));
    }
    if d.abr_samples >= 0 {
        s.push_str(&format!(" · n={}", d.abr_samples));
    }
    s
}

/// The buffer as DYNAMICS rather than a level: how much content is banked, which way it is going,
/// and — the number the whole fallback rule is written in — how long until it runs out.
///
/// `no deficit` is not the same claim as "safe forever": it is the model saying the drain rate is
/// not positive, so `T_starve = B·R/(R−C)` has no root and there is no horizon to print.
fn abr_buffer(d: &crate::player::Diag) -> String {
    if d.abr_buffer_ms < 0 {
        return "waiting for first measurement".to_string();
    }
    format!(
        "{:.1} s · {:+.1} s/s · {}",
        d.abr_buffer_ms as f64 / 1_000.0,
        d.abr_slope_ms_per_s as f64 / 1_000.0,
        match d.abr_starve_secs {
            n if n >= 0 => format!("starves in {n} s"),
            _ => "no deficit".to_string(),
        }
    )
}

/// The PMS production constraint, which is a different resource from the link and is the reason
/// 4K can be refused on a network that would carry it: the measured 4K point costs 4% more wire
/// and 110% more server.
///
/// Measured is what the last segment cost as a multiple of its own media duration; predicted is
/// what the current candidate is expected to cost, extrapolated through the raster-driven load
/// model. Both above 1.0x means the encoder is at or behind real time and will drain any buffer.
fn abr_server_load(d: &crate::player::Diag) -> String {
    if d.abr_mode != crate::player::ABR_MODE_HLS || d.abr_ratio_pm < 0 {
        return "no encoder — progressive transfer".to_string();
    }
    let mut s = format!("{:.2}x measured", d.abr_ratio_pm as f64 / 1_000.0);
    if d.abr_pred_pm >= 0 {
        s.push_str(&format!(" · {:.2}x predicted", d.abr_pred_pm as f64 / 1_000.0));
    }
    s
}

/// Auto owns a small canonical ladder; spelling the raster beside its nominal rate makes it
/// immediately obvious whether the observed decoded frame agrees with the requested rendition.
///
/// Resolved through `abr::Rung` itself rather than a table restated here. It WAS a table, and it
/// went stale the moment the ladder grew from six actuators to thirteen: every new 1080p rung
/// printed `unknown raster` on the one surface whose whole purpose is being photographed by someone
/// diagnosing a television nobody here owns.
fn abr_raster(kbps: i64) -> &'static str {
    let Ok(kbps) = u32::try_from(kbps) else { return "unknown raster" };
    let Some(rung) = crate::abr::LADDER.iter().find(|r| r.kbps() == kbps) else {
        return "unknown raster";
    };
    match rung.raster() {
        (426, 240) => "240p",
        (854, 480) => "480p",
        (1280, 720) => "720p",
        (1920, 1080) => "1080p",
        (3840, 2160) => "4K",
        _ => "unknown raster",
    }
}

fn abr_rate(kbps: i64) -> String {
    if kbps <= 0 {
        "unknown".to_string()
    } else if kbps >= 1_000 {
        format!("{:.1} Mbps", kbps as f64 / 1_000.0)
    } else {
        format!("{kbps} kbps")
    }
}

/// The operating point, and — the half that is new — the one the MODEL would choose for this link
/// if hysteresis were free. Reading them together is the difference between "this is 4 Mbps" and
/// "this is 4 Mbps and the model agrees that is all the link is worth", which are the two answers
/// a viewer asking why the picture is soft actually needs to tell apart.
fn abr_quality(d: &crate::player::Diag) -> String {
    match d.abr_mode {
        crate::player::ABR_MODE_ORIGINAL => format!("Original · source {}", abr_rate(d.abr_kbps)),
        crate::player::ABR_MODE_HLS => {
            let now = format!("HLS {} · {}", abr_rate(d.abr_kbps), abr_raster(d.abr_kbps));
            match d.abr_optimal_kbps {
                // Not "the top rung" — the best SUSTAINABLE one, which on a slow link is below
                // what is playing and is then the read-out saying a downshift is coming.
                n if n >= 0 && n != d.abr_kbps => {
                    format!("{now} → best {} · {}", abr_rate(n), abr_raster(n))
                }
                n if n >= 0 => format!("{now} (best available)"),
                _ => now,
            }
        }
        _ => "fixed — no adaptation".to_string(),
    }
}

fn abr_decision(d: &crate::player::Diag) -> String {
    if d.abr_mode == crate::player::ABR_MODE_ORIGINAL {
        // The count is CONSECUTIVE MEASUREMENT WINDOWS whose starvation horizon sits inside the
        // unsafe band — "how long has this been going on" — and the read-out says that rather than
        // `n/2`. There is no denominator to print any more: a fallback is decided by the horizon in
        // seconds and by utility, not by reaching a fixed number of bad windows, and a fraction
        // implies a countdown that would be a lie in both directions (an imminent starvation acts
        // on the FIRST window; a shortfall with a deep reserve never acts at all).
        return match d.abr_bad_windows {
            0 => "watching · link sustainable".to_string(),
            1 => "shortfall · 1 window".to_string(),
            n => format!("shortfall · {n} windows"),
        };
    }
    let target = abr_rate(d.abr_target_kbps);
    let action = match d.abr_action {
        crate::player::ABR_ACTION_STEADY => "steady".to_string(),
        crate::player::ABR_ACTION_PRIME_DOWN => format!("priming down to {target}"),
        crate::player::ABR_ACTION_PRIME_UP => format!("priming up to {target}"),
        crate::player::ABR_ACTION_COMMIT_DOWN => format!("changed down to {target}"),
        crate::player::ABR_ACTION_COMMIT_UP => format!("changed up to {target}"),
        crate::player::ABR_ACTION_REJECT_DOWN => format!("kept current · rejected {target}"),
        crate::player::ABR_ACTION_REJECT_UP => format!("kept current · rejected {target}"),
        crate::player::ABR_ACTION_PROBE_ORIGINAL => "checking Original link".to_string(),
        crate::player::ABR_ACTION_RECOVER_ORIGINAL => "switching back to Original".to_string(),
        _ => "starting".to_string(),
    };
    // The reason code and the risk score. The whole point of the rewrite is that a decision is
    // EXPLAINABLE, and until now the only place that explanation existed was the event log — which
    // is exactly the surface a user photographing their television cannot reach.
    let mut s = action;
    if d.abr_risk >= 0 {
        s.push_str(&format!(" · risk {}", d.abr_risk));
    }
    if let Some(why) = abr_why_text(d.abr_why) {
        s.push_str(" · ");
        s.push_str(why);
    }
    s
}

/// The reason code in words a reader who has never seen this codebase can act on. `None` for
/// "nothing has decided yet", which is a real state at the top of a playback rather than a fault.
fn abr_why_text(why: u8) -> Option<&'static str> {
    match why {
        crate::player::ABR_WHY_SAFE_BUDGET => Some("link has room"),
        crate::player::ABR_WHY_UNSAFE_STATE => Some("link too slow"),
        crate::player::ABR_WHY_PRODUCTION => Some("server behind"),
        crate::player::ABR_WHY_BUFFER => Some("reserve low"),
        _ => None,
    }
}

/// AUs per second per lane since the previous sample, as ` · +24/+0 /s`. Empty until there IS a
/// previous sample, and empty if the clock went backwards (an SDL tick wrap) rather than printing
/// a negative rate that would read as a fault.
/// The video half is AUs per second, which for video IS the frame rate — so it is labelled `fps`
/// and a reader can check it against the content without knowing anything about this app. The
/// audio half is fixed by the codec (AC3 packs 1536 samples, so ~31/s at 48 kHz), not by the
/// content, so it gets no unit that would imply otherwise. It read `AU/s` until someone outside
/// the project asked what an AU was.
fn fed_rate(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> String {
    match fed_rates(d, prev, now) {
        Some((rv, ra)) => format!("{rv} fps · {ra}/s"),
        None => "—".to_string(),
    }
}

/// AUs/second per lane since the previous sample. ONE derivation, shared by the Fed row and the
/// chart, so the number and the bar can never disagree.
///
/// `None` — not `(0, 0)` — when there is no previous sample: a lane that has genuinely stopped
/// also reads zero, and the two must not be the same value.
fn fed_rates(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> Option<(i64, i64)> {
    let (pv, pa, at) = prev;
    if at == 0 || now <= at {
        return None;
    }
    let dt = (now - at) as f64 / 1000.0;
    Some((
        ((d.fed_v - pv).max(0) as f64 / dt).round() as i64,
        ((d.fed_a - pa).max(0) as f64 / dt).round() as i64,
    ))
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

/// The Server row's value: "<release> · Plex Pass" / "<release> · no Plex Pass", or
/// "not yet queried" while the `GET /` worker has not landed (an empty version IS that state —
/// `serverinfo` stores the two fields together).
///
/// A server that answered but never named its subscription (a PMS predating the field) shows its
/// release alone: the row must not claim a Pass state the server did not state. Pure so every arm
/// is host-testable without touching the process-global store.
fn server_line(version: &str, sub: crate::plex::serverinfo::Subscription) -> String {
    use crate::plex::serverinfo::Subscription as S;
    if version.is_empty() {
        return "not yet queried".to_string();
    }
    let v = short_version(version);
    match sub {
        S::Yes => format!("{v} · Plex Pass"),
        S::No => format!("{v} · no Plex Pass"),
        S::Unknown => v.to_string(),
    }
}

/// The PMS release triplet ("1.43.3.10861-cd85035e7" → "1.43.3"). The panel prints THIS, not the
/// build string, and the reason is measured, not aesthetic: the value column elides at 292 px
/// from the RIGHT, and the full form ("PMS 1.43.3.10861-… · no Plex Pass", 444 px in the deployed
/// font at `size::BODY`) would elide away exactly the Pass verdict the row exists to carry, while
/// the triplet form (277 px) fits whole. The build+hash carry no support signal a release number
/// does not — and the event log's `pms: version=…` line keeps the full string for anyone who
/// needs it.
fn short_version(version: &str) -> &str {
    let numeric = version.split('-').next().unwrap_or(version);
    match numeric.match_indices('.').nth(2) {
        Some((i, _)) => &numeric[..i],
        None => numeric,
    }
}

/// Bytes as MiB, for a row that groups several and shares one unit. The divisor lives HERE with
/// [`mb`] so the module has one place that knows it.
fn mb_f(b: i64) -> f64 {
    b as f64 / (1 << 20) as f64
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
const PAD: f32 = 24.0;
/// Height of the header block. It is a SUM, not a guess: title band (14 + 36) + one identity
/// caption (26) + the verdict (30) + air. The rows start at exactly this offset, so it has to
/// clear the last thing the header DRAWS, not the last thing it is nominally made of — it was 122
/// once and the verdict line drew straight through the first row.
///
/// It carries one caption line where it carried two: the build and the firmware are one sentence
/// now, which is how they are read anyway ("PlxNative 0.4.1 dev on webOS 4.10.2").
const HEAD_H: f32 = 116.0;
/// The fed-rate chart's band, reserved by [`panel_rect`] so the rows and the chart cannot overlap
/// — the chart used to be laid out in "whatever the right column had left", which is a quantity
/// that goes negative the moment the other column is the short one.
const CHART_H: f32 = 66.0;

/// The panel's box, SIZED TO ITS CONTENT rather than to the screen.
///
/// Two consequences, and both REMOVE code rather than adding it. The video stays visible around it,
/// which is the point of a stats overlay you watch playback under — the first version was a
/// full-screen opaque card that made "is anything on screen?" unanswerable while the panel was up.
/// And it sits entirely ABOVE the transport (`player_hud::CTRL_Y`), so a pointer click can never
/// land on the scrubber's rects THROUGH an opaque card — which was the only reason the click path
/// needed a close-on-click arm at all.
fn panel_rect() -> Rect {
    // Measured on every snapshot rather than fixed at the budget: values may wrap, so
    // [`PANEL_ROWS`] is only the floor. `wrapped_line_count` is the SAME rule the list draws with
    // (`widgets::value_lines`), which is what makes this height correct rather than approximately
    // correct — it was two rules before, and the rows the measure reserved but the paint never
    // emitted fell out of the bottom of the card onto the transport.
    let w = FIELD_COL_W + 2.0 * PAD;
    let rows = unsafe { &*addr_of_mut!(ROWS) };
    let lines = FieldList::wrapped_line_count(rows, FIELD_COL_W).max(PANEL_ROWS);
    // The cap is the safe area, not a content decision — but the panel is now sized so that
    // reaching it means something has gone wrong with the content, not with the layout.
    let max_h = crate::ui::player_hud::CTRL_Y - MARGIN - PAD;
    let h = (HEAD_H + FieldList::height(lines) + CHART_H + PAD).min(max_h);
    // x on the app's own side margin, not [`MARGIN`]: the panel's whole output format is a
    // PHOTOGRAPH of a television, so it is the one overlay that must sit inside the overscan frame
    // even though nothing on it is pressable. 60 cleared it vertically and missed it by 36 across.
    Rect::new(crate::ui::consts::MARGIN_X, MARGIN, w, h)
}

/// The read-out's frame, for the overscan audit ([`crate::ui::consts::SAFE`]). Fixed at the row
/// budget by [`panel_rect`], so this is the whole state space.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    out.push(("stats read-out panel", panel_rect()));
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
    // Build + firmware as ONE identity line. Two lines was one more than the fact needs.
    if let Ok(cs) = CString::new(head[0].as_str()) {
        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
            .draw(p, Rect::new(inner, frame.y + 52.0, iw, 26.0));
    }
    // the verdict — the one line that says what the pipeline thinks it is doing
    if let Ok(cs) = CString::new(head[1].as_str()) {
        let ink = if head[1].starts_with("Playback error") { theme::DANGER } else { theme::TEXT_PRIMARY };
        Label::new(cs.as_ptr(), theme::size::BODY, ink)
            .bold()
            .draw(p, Rect::new(inner, frame.y + 80.0, iw, 30.0));
    }

    let top = frame.y + HEAD_H;
    let rows = unsafe { &*addr_of_mut!(ROWS) };
    FieldList::new(rows, Rect::new(inner, top, FIELD_COL_W, frame.h - HEAD_H - PAD)).draw(&e, p);

    // The chart sits in the band `panel_rect` reserved for it, under the rows. Its top is measured
    // from the SAME line count the list drew with, so a wrapped row pushes it down instead of
    // being drawn over by it.
    let cy = top + FieldList::height(FieldList::wrapped_line_count(rows, FIELD_COL_W));
    draw_chart(p, Rect::new(inner, cy, FIELD_COL_W, (frame.y + frame.h - PAD - cy).max(0.0)));
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
    if let Ok(cs) = CString::new(format!("FEED RATE — LAST {}s", HIST_N * SAMPLE_MS as usize / 1000)) {
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

    /// The budget is the design (see [`PANEL_ROWS`]) — a read-out that outgrows it does not
    /// scroll, it draws off the bottom of a panel someone is about to photograph.
    #[test]
    fn the_read_out_never_outgrows_its_budget() {
        // Every video-plane shape and every Auto shape: the exported path states more about the
        // plane than ACB does, and Auto trades the FFmpeg row for its five model rows — neither
        // may change the budget.
        for vp in [crate::player::VP_ACB, crate::player::VP_EXPORTED, crate::player::VP_NONE] {
            for abr_mode in [0, crate::player::ABR_MODE_ORIGINAL, crate::player::ABR_MODE_HLS] {
                let d = crate::player::Diag { vp_mode: vp, abr_mode, ..Default::default() };
                let v = rows(&d, (0, 0, 0), 1_000);
                assert!(v.len() <= PANEL_ROWS, "vp={vp} abr={abr_mode}: {} rows", v.len());
            }
        }
    }

    /// **Every row must fit on ONE line**, which is what makes the panel short. A row that wraps
    /// still draws correctly (`widgets::value_lines` is shared by the measure and the paint), so
    /// nothing is hidden and nothing overflows — but it costs a row of budget silently, and the
    /// composed lines are written to fit. Grade the composition, not just the count.
    #[test]
    fn every_composed_row_fits_one_line() {
        // The widest realistic values: a three-stage codec chain, a 4K raster with a position and
        // a skew, and the full model line with every optional part present.
        let d = crate::player::Diag {
            vp_mode: crate::player::VP_EXPORTED,
            abr_mode: crate::player::ABR_MODE_HLS,
            video_w: 3_840,
            video_h: 2_160,
            pos_ns: 3_600_000_000_000,
            dur_ns: 7_200_000_000_000,
            fed_v_pts: 10_400_000_000,
            fed_a_pts: 10_000_000_000,
            abr_kbps: 20_000,
            abr_optimal_kbps: 22_000,
            abr_net_kbps: 21_400,
            abr_safe_kbps: 17_600,
            abr_unc_pm: 200,
            abr_samples: 12,
            abr_buffer_ms: 12_000,
            abr_slope_ms_per_s: -250,
            abr_starve_secs: 48,
            abr_ratio_pm: 950,
            abr_pred_pm: 1_050,
            abr_risk: 3,
            abr_why: crate::player::ABR_WHY_PRODUCTION,
            abr_action: crate::player::ABR_ACTION_PRIME_DOWN,
            abr_target_kbps: 14_000,
            cb_count: 812,
            http_status: 200,
            net_rx: 13_000_000,
            pushed_any: true,
            fed_v: 5_000,
            fed_a: 4_800,
            window_id: "_Window_Id_0".to_string(),
            place_rv: 1,
            placed_w: 3_840,
            placed_h: 2_160,
            load_completed: true,
            ..Default::default()
        };
        for f in rows(&d, (4_400, 4_000, 500), 1_000) {
            let Some(val) = f.val.as_deref() else { continue };
            let lines = crate::ui::widgets::value_lines(val, FIELD_COL_W);
            assert_eq!(lines.len(), 1, "`{}` wraps to {} lines: {val}", f.key, lines.len());
        }
    }

    /// A fresh, never-started session must read as faults, not as a healthy zero — that is the
    /// state a user photographs when nothing happens at all, and every row that can say "this did
    /// not happen" must say it.
    #[test]
    fn a_dead_session_marks_its_faults() {
        // `load_at` a full stall-window in the past: a Load that completed SECONDS ago with no
        // frame is the fault. The same session one tick after Load is NOT — see the test below.
        let d = crate::player::Diag { load_completed: true, load_at: 1_000, ..Default::default() };
        let v = rows(&d, (0, 0, 0), 1_000 + STALL_MS + 1);
        let faults: Vec<_> = v
            .iter()
            .filter(|f| f.tone == crate::ui::widgets::Tone::Fault)
            .map(|f| f.key)
            .collect();
        // One row per thing that did not happen: no video path, a Load with no callbacks, nothing
        // demuxed or fed, and no frame long after the Load completed.
        for expect in ["Plane", "Pipeline", "Feed", "Frames"] {
            assert!(faults.contains(&expect), "{expect} should read as a fault; got {faults:?}");
        }
    }

    /// The panel must sit entirely ABOVE the transport's control row. That is what makes a pointer
    /// click unambiguous — no part of the scrubber or the discs is ever underneath an opaque card —
    /// and it is why the click path needs no close-on-click arm.
    #[test]
    fn the_panel_clears_the_transport() {
        let bottom = MARGIN + HEAD_H + FieldList::height(PANEL_ROWS) + CHART_H + PAD;
        assert!(
            bottom < crate::ui::player_hud::CTRL_Y,
            "panel bottom {bottom} overlaps the control row at {}",
            crate::ui::player_hud::CTRL_Y
        );
        // through `panel_rect` itself, not a restatement of its arithmetic: its x is `MARGIN_X`
        // (the overscan side margin) while its y is `MARGIN`, and a copy here would have kept
        // spelling 60 for both.
        let p = panel_rect();
        assert!(p.x + p.w < SCR_W, "panel is wider than the screen: {}", p.x + p.w);
        assert!(crate::ui::consts::inside_safe(p), "the read-out is photographed — it must clear the overscan frame");
    }

    /// **The chart used to be deletable by a green suite.** `draw_chart` returns silently below
    /// 40 px, and it was laid out in "whatever the right column had left" — so two extra rows
    /// anywhere took its slack, it stopped drawing, and nothing failed. Its band is RESERVED by
    /// `panel_rect` now; this grades that the reservation actually survives to the draw.
    #[test]
    fn the_chart_keeps_its_band_whatever_the_rows_do() {
        for vp in [crate::player::VP_ACB, crate::player::VP_EXPORTED, crate::player::VP_NONE] {
            for abr_mode in [0, crate::player::ABR_MODE_ORIGINAL, crate::player::ABR_MODE_HLS] {
                let d = crate::player::Diag { vp_mode: vp, abr_mode, ..Default::default() };
                let v = rows(&d, (0, 0, 0), 1_000);
                // Exactly `panel_rect`'s arithmetic and `draw`'s, so the assertion is about the
                // band the chart actually gets rather than about a restatement that could agree
                // with neither: the panel GROWS with wrapped lines, so the reservation survives.
                let lines = FieldList::wrapped_line_count(&v, FIELD_COL_W);
                let h = HEAD_H + FieldList::height(lines.max(PANEL_ROWS)) + CHART_H + PAD;
                let band = h - PAD - HEAD_H - FieldList::height(lines);
                assert!(band >= 40.0, "vp={vp} abr={abr_mode}: chart band is {band}px, it would not draw");
            }
        }
    }

    /// Auto's block is the human-readable contract for the model's atomics — the five rows are the
    /// controller's own inputs and its verdict, and this pins what each of them SAYS. Both phases:
    /// the Original watchdog must expose the evidence accumulating toward a fallback, and
    /// fixed-session HLS must expose its operating point, both resource constraints and the reason
    /// the last decision went the way it did.
    #[test]
    fn the_model_block_states_every_input_it_decides_on() {
        let val = |v: &[Field], key: &str| {
            v.iter().find(|f| f.key == key).and_then(|f| f.val.as_deref()).unwrap_or("missing").to_string()
        };
        let build = |d: &crate::player::Diag| {
            let mut v = Vec::new();
            abr_rows(d, &mut v);
            v
        };

        let original = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_ORIGINAL,
            abr_kbps: 11_356,
            abr_net_kbps: 4_016,
            abr_safe_kbps: 3_800,
            abr_unc_pm: 180,
            abr_samples: 5,
            abr_buffer_ms: 2_820,
            abr_slope_ms_per_s: -900,
            abr_starve_secs: 3,
            abr_bad_windows: 1,
            ..Default::default()
        };
        let v = build(&original);
        assert_eq!(val(&v, "Quality"), "Original · source 11.4 Mbps");
        assert_eq!(val(&v, "Link"), "safe 3.8 Mbps · measured 4.0 Mbps ±18% · n=5");
        // Level, direction and horizon: the buffer is DYNAMICS, and the fallback rule is written
        // in the third of those three numbers.
        assert_eq!(val(&v, "Buffer"), "2.8 s · -0.9 s/s · starves in 3 s");
        assert_eq!(val(&v, "Server load"), "no encoder — progressive transfer");
        // Windows, with no denominator: there is no fixed count to reach any more, so a fraction
        // would promise a countdown that does not exist in either direction.
        assert_eq!(val(&v, "Decision"), "shortfall · 1 window");
        assert_eq!(
            val(&build(&crate::player::Diag { abr_bad_windows: 4, ..original }), "Decision"),
            "shortfall · 4 windows",
        );
        // A horizon at all is a fault tint — that is the row a reader must reach first.
        assert_eq!(
            v.iter().find(|f| f.key == "Buffer").unwrap().tone,
            crate::ui::widgets::Tone::Fault,
        );

        let hls = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_kbps: 4_000,
            abr_optimal_kbps: 10_000,
            abr_net_kbps: 12_400,
            abr_safe_kbps: 11_000,
            abr_unc_pm: 200,
            abr_samples: 7,
            abr_buffer_ms: 6_250,
            abr_slope_ms_per_s: 120,
            abr_starve_secs: -1,
            abr_ratio_pm: 420,
            abr_pred_pm: 900,
            abr_risk: 0,
            abr_why: crate::player::ABR_WHY_SAFE_BUDGET,
            abr_action: crate::player::ABR_ACTION_COMMIT_UP,
            abr_target_kbps: 4_000,
            ..Default::default()
        };
        let v = build(&hls);
        // Current AND what the model would pick — the pair is the point.
        assert_eq!(val(&v, "Quality"), "HLS 4.0 Mbps · 720p → best 10.0 Mbps · 1080p");
        assert_eq!(val(&v, "Link"), "safe 11.0 Mbps · measured 12.4 Mbps ±20% · n=7");
        assert_eq!(val(&v, "Buffer"), "6.2 s · +0.1 s/s · no deficit");
        assert_eq!(val(&v, "Server load"), "0.42x measured · 0.90x predicted");
        assert_eq!(val(&v, "Decision"), "changed up to 4.0 Mbps · risk 0 · link has room");

        // Every actuator on the ladder names its raster, including the ones added after this panel
        // was written — the failure mode is a photograph reading `unknown raster`.
        for rung in crate::abr::LADDER {
            let probe = crate::player::Diag {
                abr_mode: crate::player::ABR_MODE_HLS,
                abr_kbps: i64::from(rung.kbps()),
                abr_optimal_kbps: -1,
                ..Default::default()
            };
            assert!(!val(&build(&probe), "Quality").contains("unknown"), "{rung:?} has no raster name");
        }

        let probing = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_risk: -1,
            abr_action: crate::player::ABR_ACTION_PROBE_ORIGINAL,
            ..Default::default()
        };
        assert_eq!(val(&build(&probing), "Decision"), "checking Original link");
        let recovering = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_risk: -1,
            abr_action: crate::player::ABR_ACTION_RECOVER_ORIGINAL,
            ..Default::default()
        };
        assert_eq!(val(&build(&recovering), "Decision"), "switching back to Original");

        // A fixed rung has no model at all — the block collapses to the one fact worth a row.
        let fixed = build(&crate::player::Diag::default());
        assert_eq!(fixed.len(), 1);
        assert_eq!(fixed[0].key, "FFmpeg");
        for absent in ["Quality", "Link", "Buffer", "Server load", "Decision"] {
            assert!(!fixed.iter().any(|f| f.key == absent), "{absent} must not draw without Auto");
        }
    }

    /// …and it must leave the MAJORITY of the picture visible, or it is the full-screen card
    /// again and "is anything on screen?" — the question, when playback is broken — stops being
    /// answerable while the read-out is up. 40% is the line rather than a third: the codec rows
    /// and the chart are worth the four points, and a corner panel at 35% still shows most of the
    /// frame. What is NOT negotiable is that it stays a corner panel; if this ever needs raising
    /// again, shrink the type instead.
    #[test]
    fn it_leaves_most_of_the_picture_visible() {
        let a = (FIELD_COL_W + 2.0 * PAD)
            * (HEAD_H + FieldList::height(PANEL_ROWS) + CHART_H + PAD);
        let pct = 100.0 * a / (SCR_W * SCR_H);
        // One column at 30px rows costs about 27% where two at 36px cost 34%. The ceiling stays
        // where it was rather than being tightened onto today's number: a row added tomorrow
        // should cost a row, not a redesign.
        assert!(pct < 40.0, "panel covers {pct:.0}% of the screen");
    }

    /// THE case this pair exists for: "video plays but there is no sound after scrubbing". The
    /// audio lane stopped 30 s ago, so its TOTAL is still large — every instantaneous field reads
    /// healthy — and only the rate and the skew can see it.
    #[test]
    fn a_stalled_audio_lane_is_visible_even_though_its_total_is_large() {
        let d = crate::player::Diag {
            load_completed: true,
            pushed_any: true,
            fed_v: 5_000,
            fed_a: 4_000,          // large, and unmoved since the previous sample
            fed_v_pts: 60_000_000_000,
            fed_a_pts: 30_000_000_000, // 30 s behind
            ..Default::default()
        };
        let v = rows(&d, (4_400, 4_000, 500), 1_000);
        let feed = v.iter().find(|f| f.key == "Feed").expect("Feed row");
        let val = feed.val.as_deref().unwrap_or_default();
        assert!(val.contains("1200 fps · 0/s"), "the rate must show the dead lane: {val}");

        // The skew rides the Frame row — same stream, and it is read beside the raster and the
        // position rather than as a number on its own.
        let frame = v.iter().find(|f| f.key == "Frame").expect("Frame row");
        let fv = frame.val.as_deref().unwrap_or_default();
        assert!(fv.contains("A/V +30.0 s"), "{fv}");
        assert_eq!(frame.tone, crate::ui::widgets::Tone::Fault, "30 s of skew is a fault");
    }

    /// The clock is what makes "no frames" mean something. One tick after `loadCompleted` a
    /// frameless pipeline is a pipeline that has not started yet; eight seconds later it is a
    /// stall. Without this distinction the panel cries wolf on every single playback.
    #[test]
    fn a_freshly_completed_load_with_no_frames_yet_is_not_a_fault() {
        // Serialized: `frames_str` reads the process-wide `player::TX.paused`, which the paused
        // test below toggles under this same lock — without it, this test can observe the paused
        // branch ("none yet") where it asserts the running clock ("none in 0 s") and flake.
        let _g = crate::testlock::serial();
        let d = crate::player::Diag { load_completed: true, load_at: 1_000, ..Default::default() };
        let fresh = rows(&d, (0, 0, 0), 1_100);
        let f = fresh.iter().find(|f| f.key == "Frames").unwrap();
        assert_ne!(f.tone, crate::ui::widgets::Tone::Fault, "0.1 s after Load is not a stall");
        assert!(f.val.as_deref().unwrap().starts_with("none in 0 s"));

        let stalled = rows(&d, (0, 0, 0), 1_000 + STALL_MS + 4_000);
        let f = stalled.iter().find(|f| f.key == "Frames").unwrap();
        assert_eq!(f.tone, crate::ui::widgets::Tone::Fault, "12 s after Load with no frame IS");
        assert!(f.val.as_deref().unwrap().starts_with("none in 12 s"));
    }

    /// A paused picture is not a stalled one. The verdict line disarms its stall clock while
    /// paused; this pins that the Frames row agrees, because a panel that contradicts itself sends
    /// its reader after a fault that is just the pause button. Reported from the wild on 0.2.1.
    #[test]
    fn a_paused_stream_does_not_report_its_frames_as_frozen() {
        let _g = crate::testlock::serial();
        let d = crate::player::Diag {
            load_completed: true, seen_frame: true, frames: 190, frame_at: 1_000, ..Default::default()
        };
        let long_after = 1_000 + STALL_MS + 8_000;

        crate::player::TX.paused.store(true, Ordering::Relaxed);
        let paused = rows(&d, (0, 0, 0), long_after);
        crate::player::TX.paused.store(false, Ordering::Relaxed);
        let playing = rows(&d, (0, 0, 0), long_after);

        let f = |v: &Vec<Field>| v.iter().find(|f| f.key == "Frames").unwrap().val.clone().unwrap();
        assert!(f(&paused).starts_with("190 · "), "paused: just the count, {}", f(&paused));
        assert!(f(&playing).contains("frozen"), "playing and not advancing IS frozen: {}", f(&playing));
    }

    /// The transport row splits a class the panel could not previously see at all: no connection,
    /// a connection that was refused, and a connection that answered and delivered nothing.
    #[test]
    fn the_http_row_splits_the_open_failures() {
        let row = |d: &crate::player::Diag| {
            rows(d, (0, 0, 0), 1_000).into_iter().find(|f| f.key == "Pipeline").unwrap()
        };
        let none = row(&crate::player::Diag::default());
        assert!(none.val.as_deref().unwrap().ends_with("no connection"));

        let refused = row(&crate::player::Diag { http_status: 401, ..Default::default() });
        assert!(refused.val.as_deref().unwrap().contains("HTTP 401 · 0 B"));
        assert_eq!(refused.tone, crate::ui::widgets::Tone::Fault);

        // answered fine and delivered bytes — the fault is downstream, and this row says so
        let ok = row(&crate::player::Diag {
            http_status: 200,
            net_rx: 13_000_000,
            cb_count: 4,
            ..Default::default()
        });
        assert!(ok.val.as_deref().unwrap().contains("HTTP 200 · 12.4 MB"));
        assert_ne!(ok.tone, crate::ui::widgets::Tone::Fault);
    }

    /// `queue empty` vs `BufferFull` is the row's whole purpose: a dead PRODUCER and a dead SINK
    /// read identically everywhere else on the panel and want opposite fixes. Neither is a fault
    /// tint — both are ordinary moments in a healthy stream; only an outright refusal is.
    #[test]
    fn the_feed_row_splits_a_dead_producer_from_a_dead_sink() {
        let f = |st: u8| crate::player::Diag { feed_state: st, ..Default::default() };
        assert_eq!(f(5).feed_state_str(), "queue empty (no data)");
        assert_eq!(f(2).feed_state_str(), "BufferFull (sink is full)");
        assert!(!f(5).feed_is_fault() && !f(2).feed_is_fault() && !f(4).feed_is_fault());
        assert!(f(3).feed_is_fault(), "an outright refusal is the only fault");
    }

    /// A latched pipeline error must survive later healthy callbacks — it is the one event that
    /// explains the session — and must carry WHERE it happened, so "refused immediately" and
    /// "died after a long healthy run" are different readings.
    #[test]
    fn a_latched_pipeline_error_outranks_a_healthy_callback_count() {
        let d = crate::player::Diag { cb_count: 812, cb_err: 18, cb_err_at: 4, load_completed: true, ..Default::default() };
        let row = rows(&d, (0, 0, 0), 1_000).into_iter().find(|f| f.key == "Pipeline").unwrap();
        assert!(row.val.as_deref().unwrap().contains("812 cb · ERR 18 @ 4"));
        assert_eq!(row.tone, crate::ui::widgets::Tone::Fault);
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

    /// The Server row's three arms (issue #22's blind spot made visible). A free server is a FACT
    /// the row states plainly — the fault-tone assertion lives with the row builder test below —
    /// and a server that never named its subscription must not be assigned one either way.
    #[test]
    fn the_server_row_states_the_pass_tristate_without_guessing() {
        use crate::plex::serverinfo::Subscription as S;
        assert_eq!(server_line("1.43.3.10861-cd85035e7", S::Yes), "1.43.3 · Plex Pass");
        assert_eq!(server_line("1.43.3.10861-cd85035e7", S::No), "1.43.3 · no Plex Pass");
        // fetch never landed: version and subscription are stored together, so empty = unqueried
        assert_eq!(server_line("", S::Unknown), "not yet queried");
        // answered, but a PMS old enough not to carry the field: the release alone, no claim
        assert_eq!(server_line("0.9.12.4", S::Unknown), "0.9.12");
    }

    /// The truncation exists for the elide (see [`short_version`]) and must survive whatever
    /// shape a server's version string takes — a short form must pass through, never panic.
    #[test]
    fn the_version_triplet_survives_every_shape() {
        assert_eq!(short_version("1.43.3.10861-cd85035e7"), "1.43.3");
        assert_eq!(short_version("1.43.3"), "1.43.3");
        assert_eq!(short_version("1.43"), "1.43");
        assert_eq!(short_version("1.43-beta.2.1"), "1.43");
    }

    /// A free server reads as a fact, never a fault: the danger tint is reserved for rows that say
    /// "something broke", and `no Plex Pass` is the server working exactly as sold. (The value
    /// itself depends on the process-global store this test deliberately does not touch — the
    /// tone is a property of the ROW, fixed at build time, so it is assertable regardless.)
    #[test]
    fn the_server_row_is_never_a_fault() {
        // It shares the Source row now — the delivery kind and the server that produced it are one
        // sentence — so the assertion is that THAT row never carries the tint.
        let d = crate::player::Diag::default();
        let row = rows(&d, (0, 0, 0), 1_000).into_iter().find(|f| f.key == "Source").expect("Source row");
        assert_ne!(row.tone, crate::ui::widgets::Tone::Fault);
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
        let head = header(&crate::player::Diag::default(), 1_000);
        // The firmware rides the IDENTITY line now (head[0]); head[1] is the verdict, and the two
        // being one array is what stops the firmware taking the verdict's slot again.
        let line = &head[0];
        if crate::webos::info().major == 0 {
            assert!(line.contains("unknown"), "{line}");
        } else {
            assert!(line.contains("webOS "), "{line}");
        }
        assert!(line.contains("surface "), "the identity line carries the drawable: {line}");
        // The verdict is a PLAYBACK state, never a firmware string — the regression this pins is
        // the firmware line being drawn where the failure reason belongs.
        assert!(!head[1].contains("webOS"), "the verdict slot must not carry the firmware: {}", head[1]);
        assert_eq!(head[1], "Idle", "a default Diag with no session is Idle");
    }
}
