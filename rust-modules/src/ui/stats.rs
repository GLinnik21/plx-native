//! **Stats for nerds** — the on-screen diagnostics read-out, toggled from the player's `…` overflow
//! popover ([`crate::ui::more_menu`]) and, since 2026-08-29, from the account menu
//! ([`crate::ui::account_menu`]) on every other route.
//!
//! It was PLAYER-ONLY until then, and this doc said so: `app.rs` drew it inside the player branch,
//! so a toggle offered anywhere else would have ticked a box and shown nothing. The gap that
//! paragraph named — *"starts but finds no server", which never reaches a player* — is the report
//! this app most often needs from a stranger and was the one it could not produce. The draw call
//! now runs on both sides of `app.rs`'s player/non-player split, and the `else` half covers every
//! other route at once, so no route can be forgotten. The two positions differ deliberately: on the
//! player it is genuinely last, and off it the panel draws over the PAGE but UNDER the app's modal
//! surfaces — because on that path something sits in its corner, namely the account popover that
//! carries the row turning it off. Drawn last it hid its own off-switch.
//!
//! **Off the player the panel shows a DIFFERENT set of rows, and that is not a decoration.** Every
//! pipeline row reads a [`crate::player::Diag`] that has never been filled in, so all nine of them
//! report a zero — `stream never opened`, `no callbacks`, `NOTHING demuxed` — and most carry a
//! fault tint. Photographed on a Home screen where nothing is wrong, that is a picture of a broken
//! app, which is the exact opposite of what this panel is for. So when nothing has been asked to
//! play this session ([`never_played`]) the list is [`device_rows`] instead: what the SET is, what
//! its decoder claims, and what the server said — the facts a "it just does not work on my TV"
//! report needs and no log a stranger can reach will ever carry.
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
//! **It must remain complete in a phone photo.** This is an engineering instrument opened on
//! purpose, so it uses the diagnostics-only 20px face over a near-black opaque ground: density and
//! stable coordinates matter more here than the product UI's couch-copy scale. No value is elided.
//! Pixel wrapping is cached with the 2 Hz snapshot, the panel grows to the measured lines, and an
//! oversized opaque token is split rather than losing its suffix. Severity is still carried by a
//! WORD as well as colour because a phone camera chroma-subsamples the result.
//!
//! **It must not become the screen.** It is a wide, shallow card inside the full overscan-safe
//! width, leaving most of the picture and the whole transport visible. Its two columns have fixed
//! schemas: stream/output on the left and delivery/control on the right. Manual, Auto/Original and
//! Auto/HLS replace values in place; they never remove rows and slide unrelated evidence around.
//! It never scrolls — a read-out that needs two photographs cannot be compared at one instant.
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
//! The enforcement is structural: every row is built by [`pipeline_rows`]/[`model_rows`] from one
//! [`crate::player::Diag`] snapshot and a small set of route facts. The compositor's opaque window
//! id is reduced to `window ready`/`NO WINDOW`; the only server-derived text is the PMS release and
//! Pass state, never its name or address. There is no generic "push a string to diagnostics" path,
//! so adding a field is a deliberate edit to the file that carries these rules.
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
/// The schema is fixed across fixed quality, Auto/Original and Auto/HLS.  A mode may say that a
/// value is inactive, but it may not delete the row and slide unrelated evidence into its place —
/// photographs from two modes must be directly comparable by y-coordinate.
const LEFT_ROWS: usize = 13;
const RIGHT_ROWS: usize = 9;
/// Compatibility name for the support-panel budget and its existing host assertions. Playback
/// uses [`LEFT_ROWS`] as the taller of the two fixed columns; the pre-playback device card uses
/// only the rows it actually has.
const PANEL_ROWS: usize = LEFT_ROWS;
/// The chart occupies the right column's remaining four row pitches.  Naming the budget makes the
/// two columns exactly the same height without a guessed pixel remainder.
const CHART_ROWS: usize = LEFT_ROWS - RIGHT_ROWS;

/// The previous sample's fed totals and the tick they were taken at — what turns two totals into
/// a RATE. Without it the panel can only say "1180 AUs have been fed", which stays true and stays
/// large for as long as the app runs, including for the whole of a lane that stopped feeding
/// thirty seconds ago. `(video, audio, at)`; `at == 0` means there is no previous sample yet.
static mut PREV_FED: (i64, i64, u32) = (0, 0, 0);
/// The sweep plot is 32 fixed physical cells. A new sample overwrites the cell under the cursor;
/// the already-drawn shape does not shift left. That is the useful property of YouTube's Stats for
/// Nerds plot: two phone photographs retain a stable x-coordinate system, and the bright cursor
/// says which side of it is new rather than making the reader mentally follow a scrolling queue.
///
/// Sixteen seconds at 2 Hz is long enough to show a segment acquisition, a quality transaction and
/// its buffer consequence while keeping every cell wide enough to survive a phone camera.
const HIST_N: usize = 32;
const CHART_LABELS: [&str; 3] = ["BUDGET / DEMAND", "NETWORK ACTIVITY", "BUFFER HEALTH"];
/// Conservative paint width before SDL_ttf is ready. The live path measures the same three runs
/// the chart draws; this fallback is the widest one's 1920×1080 simulator measurement rounded up.
const CHART_LABEL_FALLBACK_PX: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SweepSample {
    valid: bool,
    budget_kbps: i64,
    demand_kbps: i64,
    activity_kbps: i64,
    buffer_ms: i64,
}

impl SweepSample {
    const EMPTY: Self = Self {
        valid: false,
        budget_kbps: -1,
        demand_kbps: -1,
        activity_kbps: -1,
        buffer_ms: -1,
    };
}

#[derive(Clone, Copy)]
struct SweepHistory {
    slots: [SweepSample; HIST_N],
    /// The next physical slot to overwrite — and therefore the x-coordinate of the sweep cursor.
    head: usize,
    filled: usize,
    epoch: u32,
    has_epoch: bool,
    prev_net_rx: i64,
    prev_at: u32,
}

impl SweepHistory {
    const fn new() -> Self {
        Self {
            slots: [SweepSample::EMPTY; HIST_N],
            head: 0,
            filled: 0,
            epoch: 0,
            has_epoch: false,
            prev_net_rx: 0,
            prev_at: 0,
        }
    }

    fn reset_for(&mut self, epoch: u32) {
        self.slots = [SweepSample::EMPTY; HIST_N];
        self.head = 0;
        self.filled = 0;
        self.epoch = epoch;
        self.has_epoch = true;
        self.prev_net_rx = 0;
        self.prev_at = 0;
    }

    fn push(&mut self, mut sample: SweepSample) {
        sample.valid = true;
        self.slots[self.head] = sample;
        self.head = (self.head + 1) % HIST_N;
        self.filled = self.filled.saturating_add(1).min(HIST_N);
    }

    fn record(
        &mut self,
        epoch: u32,
        d: &crate::player::Diag,
        selected: crate::route::Quality,
        now: u32,
    ) {
        if !self.has_epoch || self.epoch != epoch {
            self.reset_for(epoch);
        }
        let activity_kbps = network_activity_kbps(d.net_rx, self.prev_net_rx, self.prev_at, now);
        self.prev_net_rx = d.net_rx.max(0);
        self.prev_at = now;
        self.push(SweepSample {
            valid: true,
            budget_kbps: d.abr_safe_kbps,
            demand_kbps: chart_demand_kbps(d, selected),
            activity_kbps,
            buffer_ms: observed_buffer_ms(d).unwrap_or(-1),
        });
    }

    fn head(&self) -> usize {
        self.head
    }

    fn slot(&self, physical: usize) -> SweepSample {
        self.slots[physical]
    }

    fn latest(&self) -> Option<SweepSample> {
        (self.filled > 0).then(|| self.slots[(self.head + HIST_N - 1) % HIST_N])
    }

    /// Arithmetic mean of the visible activity window. Segmented transfers are bursty and spend
    /// real intervals at zero, so those zeros belong in the mean; `-1` alone is an interval we did
    /// not observe and is excluded. A median would often be zero, while a high percentile would
    /// promote burst peaks into a false connection-speed estimate.
    fn mean_activity_kbps(&self) -> Option<i64> {
        let (sum, count) = self
            .slots
            .iter()
            .filter(|sample| sample.valid && sample.activity_kbps >= 0)
            .fold((0i128, 0i128), |(sum, count), sample| {
                (sum + i128::from(sample.activity_kbps), count + 1)
            });
        (count > 0).then(|| i64::try_from(sum / count).unwrap_or(i64::MAX))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkState {
    Unknown,
    Deficit,
    Sustains,
}

fn link_state(budget_kbps: i64, demand_kbps: i64) -> LinkState {
    if budget_kbps < 0 || demand_kbps <= 0 {
        LinkState::Unknown
    } else if budget_kbps < demand_kbps {
        LinkState::Deficit
    } else {
        LinkState::Sustains
    }
}

static mut HISTORY: SweepHistory = SweepHistory::new();
/// Formatted only at the 2 Hz sampling boundary, never in the render loop. The whole-string glyph
/// cache therefore sees the same held values as the ordinary rows.
static mut CHART_VALUES: [String; 3] = [String::new(), String::new(), String::new()];
/// SDL_ttf measurement of [`CHART_LABELS`] plus their sibling gap. `text_width` is uncached, so the
/// fixed labels are measured once rather than three times on every presented playback frame.
static mut CHART_KEY_W: f32 = 0.0;
static mut NEXT_SAMPLE: u32 = 0;
static mut COLUMNS: [Vec<Field>; 2] = [Vec::new(), Vec::new()];
/// The compact pre-playback device read-out. Playback itself uses [`COLUMNS`]. Keeping the two
/// snapshots separate lets the panel change width without ever measuring one schema and drawing
/// the other on the transition frame.
static mut ROWS: Vec<Field> = Vec::new();
static mut HEAD: [String; 2] = [String::new(), String::new()];
/// Which list [`ROWS`] currently holds: the device block ([`device_rows`]) rather than the
/// pipeline one. Sampled with the rows so the layout cannot disagree with the content — both
/// [`panel_rect`] and [`draw`] read it, and the card is shorter and chartless in this state.
static mut IDLE: bool = false;

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
        // ONE decision per sample, held with the rows it chose. Deciding this in `draw` instead
        // would let the panel measure one list and paint another on the frame the first Load lands.
        let idle = never_played(&d, crate::player::state());
        addr_of_mut!(IDLE).write(idle);
        drop(addr_of_mut!(ROWS).replace(if idle { device_rows() } else { Vec::new() }));
        drop(addr_of_mut!(COLUMNS).replace(if idle {
            [Vec::new(), Vec::new()]
        } else {
            columns(&d, prev, now)
        }));
        addr_of_mut!(PREV_FED).write((d.fed_v, d.fed_a, now));
        let history = &mut *addr_of_mut!(HISTORY);
        history.record(
            crate::route::playback_trace_generation(),
            &d,
            crate::route::quality(),
            now,
        );
        drop(addr_of_mut!(CHART_VALUES).replace(chart_values(history)));
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
            if cfg!(feature = "devtriggers") {
                "dev"
            } else {
                "release"
            }
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
    // The reason is part of every Error verdict — bare "Playback error" made the reviewer derive
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

/// **Has anything been asked to play this session?** — which of the two lists [`update`] samples.
///
/// `Idle` alone would be wrong, and the reason is the case that matters most: `Idle` is also where
/// a playback that FINISHED or that failed and was cleared comes to rest, and there the pipeline
/// block is exactly what a reader wants — the post-mortem. `load_at == 0` is what separates the two,
/// since it is set once per session at the moment a Load is sent and never returns to zero.
///
/// The resolve and connect stages need no clause of their own: they are Load-less but they are not
/// `Idle`, so they keep the pipeline block and its `Load waiting · no callbacks · no connection`
/// row — the one sentence that says where such an attempt is stuck.
///
/// Pure, with the process-global state passed in, so both arms are host-testable.
fn never_played(d: &crate::player::Diag, st: crate::player::PlaybackState) -> bool {
    matches!(st, crate::player::PlaybackState::Idle) && d.load_at == 0
}

/// **The read-out with no playback behind it: what this SET is.**
///
/// The report this app cannot otherwise get. Every diagnostic surface it has — the event log, the
/// ~40 triggers, the remote FIFO, ssh — needs either a rooted television or a `devtriggers` build,
/// and the one failure that reaches us most often ("it installs, it opens, it finds nothing") never
/// reaches a player at all, so until now it could produce no artefact whatsoever. These five rows
/// are photographable from a Home screen with the remote alone.
///
/// The panel's content rules are unchanged and every one of them holds here by construction: no
/// path, no URL, no credential, no stable identity. A MODEL and a BOARD are product identities
/// shared by every unit LG built — they are what a decode or plane bug correlates with, and they
/// say nothing about a household. `lab::snapshot`'s envelope has carried the same three fields
/// since it was written; this is the same rule reaching the surface a stranger can actually use.
fn device_rows() -> Vec<Field> {
    let hw = crate::webos::device();
    let i = crate::webos::info();
    let c = crate::devcaps::caps();
    let mut v = Vec::with_capacity(5);

    // WHICH SET. The question every report from hardware nobody here owns opens with, and the one
    // no log a stranger can reach has ever answered. Empty when nyx did not answer — never a
    // plausible default, which is `webos::Hardware`'s own rule for the same reason.
    let set = hw.set_line();
    let set_unknown = set.is_empty();
    v.push(
        Field::new(
            "Set",
            if set_unknown {
                "unknown — nyx did not answer".to_string()
            } else {
                set
            },
        )
        .fault(set_unknown),
    );

    // The firmware CODENAME, which is the half the header line drops for width and the key
    // webosbrew buckets its firmware by — so it is the field that turns "webOS 4.10.2" into an
    // image somebody else can go and look at.
    v.push(Field::new(
        "Firmware",
        match (i.name.as_str(), i.codename.as_str()) {
            // Bare "unknown": the head line above already carries the REASON in this state
            // ("webOS unknown — os_info.json unreadable"), and a photograph should not spend two
            // of its five rows on one fact.
            ("", "") => "unknown".to_string(),
            ("", cn) => cn.to_string(),
            (n, "") => n.to_string(),
            (n, cn) => format!("{n} · {cn}"),
        },
    ));

    // What the SoC's own table claims, and — the half that cannot be left out — WHETHER IT WAS
    // READ. `devcaps` falls back to the 49SM9000PLA profile when the table is missing or
    // unparseable, so the values alone cannot tell a measurement from this project's dev set. A
    // panel whose output is a photograph must not print the second as if it were the first.
    let measured = crate::devcaps::measured();
    v.push(
        Field::new(
            "Decoder",
            format!(
                "{} · {} · {}",
                if c.hevc {
                    format!("HEVC {}x{}", c.hevc_max.0, c.hevc_max.1)
                } else {
                    "no HEVC".to_string()
                },
                if c.vp9 { "VP9" } else { "no VP9" },
                if measured { "device table" } else { "ASSUMED" },
            ),
        )
        .fault(!measured),
    );
    v.push(Field::new("Audio", c.audio.clone()));

    // The same row the pipeline block leads with, minus the direct-play/transcode half that has no
    // meaning yet: it answers "did the server ever reply", which IS the failure when nothing plays.
    v.push(Field::new(
        "Server",
        server_line(
            &crate::plex::serverinfo::version(),
            crate::plex::serverinfo::subscription(),
        ),
    ));
    v
}

/// One fixed instrument, split by responsibility rather than by mode.  The left column follows a
/// byte from the chosen PMS connection to the television plane.  The right column is the adaptive
/// model.  Fixed playback keeps every right-hand row and says `inactive`; that stability is what
/// makes a LAN/Original photograph directly comparable with a remote/HLS one.
fn columns(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> [Vec<Field>; 2] {
    [pipeline_rows(d, prev, now), model_rows(d)]
}

/// Flattened only for host assertions that inspect the whole schema.  Production draws the two
/// vectors independently and never clones them.
fn rows(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> Vec<Field> {
    columns(d, prev, now).into_iter().flatten().collect()
}

fn pipeline_rows(d: &crate::player::Diag, prev: (i64, i64, u32), now: u32) -> Vec<Field> {
    let mut v = Vec::with_capacity(LEFT_ROWS);
    v.push(Field::new("Connection", connection_line()));
    v.push(Field::new("Route", route_line(d)));

    let mut video = chain(
        crate::route::source_vcodec(),
        crate::route::stream_vcodec(),
        d.load_v_str(),
    );
    let dv = crate::route::stream_dovi();
    if dv.present {
        video.push_str(&format!(" · Dolby Vision P{}.{}", dv.profile, dv.bl_compat));
    }
    v.push(Field::new("Video", video));

    let mut audio = chain(
        crate::route::source_acodec(),
        crate::route::stream_acodec(),
        d.load_a_str(),
    );
    if crate::route::stream_immersive() {
        audio.push_str(" · Dolby Atmos");
    }
    v.push(Field::new("Audio", audio).fault(d.load_a == 0 && d.load_v != 0));

    // **The frame rate here is a CLAIM, and the row says whose.** `video_fps_milli` is what LG's
    // pipeline announced in its own `sourceInfo` callback, and on this set that is not a
    // measurement of anything presented: a 4K H.264 24p direct play declared at 60 was announced
    // as 24 and then re-announced as 30 (2026-09-03, the maintainer's "the panel says 30 fps and
    // the picture is plainly worse"), which is the pipeline's lattice guess, not the stream. The
    // route's value is what WE told it. Nothing in this process counts presented frames (the
    // position callback ticks at 5 Hz whatever the picture does), so an unlabelled number here is
    // the silent-instrument trap on a photographed surface.
    let route_fps = crate::route::stream_fps();
    let (fps_milli, fps_src) = if d.video_fps_milli > 0 {
        (d.video_fps_milli, "pipeline says")
    } else if route_fps > 0.0 {
        ((route_fps * 1_000.0).round() as i64, "declared")
    } else {
        (0, "")
    };
    v.push(
        Field::new(
            "Picture",
            match (d.video_w, d.video_h) {
                (0, _) | (_, 0) => "stream never opened".to_string(),
                (w, h) if fps_milli > 0 => {
                    format!("{w}×{h} · {} fps ({fps_src})", fps_milli_str(fps_milli))
                }
                (w, h) => format!("{w}×{h} · fps unknown"),
            },
        )
        .fault(d.video_w == 0 || d.video_h == 0),
    );
    v.push(Field::new(
        "Timeline",
        format!(
            "{} / {}",
            crate::ui::fmt::clock(d.pos_ns / 1_000_000),
            if d.dur_ns > 0 {
                crate::ui::fmt::clock(d.dur_ns / 1_000_000)
            } else {
                "?".into()
            },
        ),
    ));
    v.push(Field::new("A/V sync", skew(d)).fault(skew_bad(d)));

    let (plane, plane_bad) = plane_line(d);
    v.push(Field::new("Video plane", plane).fault(plane_bad));

    let transfer = match (d.http_status, d.net_rx) {
        (0, _) => "no connection".to_string(),
        (st, rx) => format!("HTTP {st} · {} received", mb(rx)),
    };
    v.push(
        Field::new("Transfer", transfer)
            .fault(d.http_status != 0 && !(200..300).contains(&d.http_status)),
    );
    v.push(
        Field::new(
            "Load",
            format!(
                "{} · {}",
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
                    (n, e) => format!("{n} callbacks · ERROR {e} at #{0}", d.cb_err_at),
                },
            ),
        )
        .fault(d.load_failed || d.cb_err != 0 || (d.cb_count == 0 && d.load_completed)),
    );
    v.push(
        Field::new(
            "Feed",
            format!(
                "{} · {}",
                if d.pushed_any {
                    fed_rate(d, prev, now)
                } else {
                    "NOTHING demuxed".to_string()
                },
                d.feed_state_str(),
            ),
        )
        .fault(!d.pushed_any || (d.fed_v == 0 && d.load_completed) || d.feed_is_fault()),
    );
    let (cv, ca) = crate::player::aq_caps();
    v.push(Field::new(
        "Queues",
        format!(
            "video {:.1}/{:.1} MB · audio {:.2}/{:.1} MB",
            mb_f(d.aq_video),
            mb_f(cv),
            mb_f(d.aq_audio),
            mb_f(ca),
        ),
    ));
    v.push(
        Field::new("Frames", frames_str(d, now))
            .fault(!d.seen_frame && d.load_completed && since(d.load_at, now) > STALL_MS),
    );
    debug_assert_eq!(v.len(), LEFT_ROWS);
    v
}

fn fps_milli_str(fps_milli: i64) -> String {
    if fps_milli % 1_000 == 0 {
        (fps_milli / 1_000).to_string()
    } else {
        format!("{:.3}", fps_milli as f64 / 1_000.0)
            .trim_end_matches('0')
            .to_string()
    }
}

fn connection_line() -> String {
    let sid = crate::route::cur_sid();
    let Some(client) = crate::plex::client_for(sid) else {
        return "standalone · no PMS".to_string();
    };
    let tier = match client.link() {
        Some(crate::plex::probe::Location::Local) => "LAN",
        Some(crate::plex::probe::Location::Remote) => "remote",
        Some(crate::plex::probe::Location::Relay) => "relay",
        None => "link unknown",
    };
    format!(
        "{tier} · PMS {}",
        server_line(
            &crate::plex::serverinfo::version_of(sid),
            crate::plex::serverinfo::subscription_of(sid),
        )
    )
}

fn route_line(d: &crate::player::Diag) -> String {
    let transport = if d.abr_mode == crate::player::ABR_MODE_HLS || crate::route::is_segmented_hls()
    {
        "HLS"
    } else {
        "progressive"
    };
    let transform = match (crate::route::is_transcoding(), crate::route::is_remux()) {
        (false, _) => "direct play",
        (true, true) => "remux (stream copy)",
        (true, false) => "transcode (re-encode)",
    };
    format!("{transport} · {transform}")
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
            // The identifier itself is not useful evidence and is the only unbounded string in a
            // field row.  The state we need is whether the compositor gave us one at all.
            let win = if d.window_id.is_empty() {
                "NO WINDOW"
            } else {
                "window ready"
            };
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
            format!(
                "{mode} · {}",
                match (d.acb_ok, d.stage) {
                    (false, _) => "NOT AVAILABLE",
                    (true, 0) => "init'd · NOT bound",
                    (true, 1) => "bind sent",
                    _ => "streaming",
                }
            ),
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

/// Build the fixed right-hand schema.  `inactive` is data: it says that a manual selection owns the
/// playback and no Auto estimate should be inferred from the empty cells.  Deleting those cells
/// made every mode a different panel and is the regression this shape prevents.
fn model_rows(d: &crate::player::Diag) -> Vec<Field> {
    let mut v = Vec::with_capacity(RIGHT_ROWS);
    // One selection snapshot for the whole block.  A quality press must not leave Mode saying
    // Auto while the lower rows have already formatted the new manual state.
    abr_rows(d, crate::route::quality(), &mut v);
    debug_assert_eq!(v.len(), RIGHT_ROWS);
    v
}

fn abr_rows(d: &crate::player::Diag, selected: crate::route::Quality, v: &mut Vec<Field>) {
    v.push(Field::new("Mode", abr_mode(d, selected)));
    v.push(Field::new("Quality", abr_quality(d, selected)));
    v.push(Field::new("Sample", abr_link(d, selected)));
    v.push(Field::new("Conservative", abr_budget(d, selected)));
    v.push(Field::new("Buffer", abr_buffer(d, selected)));
    v.push(
        Field::new("Risk", abr_risk(d, selected))
            .fault(d.abr_why == crate::player::ABR_WHY_STARVATION),
    );
    v.push(Field::new("Acquisition", abr_acquisition_cadence(d)));
    v.push(Field::new("Action", abr_action(d, selected)));
    v.push(Field::new("Reason", abr_reason(d, selected)));
}

fn abr_mode(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    match d.abr_mode {
        crate::player::ABR_MODE_ORIGINAL => "Auto · Original".to_string(),
        crate::player::ABR_MODE_HLS => "Auto · HLS".to_string(),
        _ if selected == crate::route::Quality::Auto => "Auto · controller idle".to_string(),
        _ => format!("Manual · {}", selected.label()),
    }
}

/// The controller has no published state (`abr_mode == 0`).  Distinguish an explicit manual
/// selection from Auto being selected on a path that did not arm an adaptive session; calling the
/// latter "fixed by user" is exactly the contradiction the simulator screenshot exposed.
fn inactive_model(selected: crate::route::Quality, absent: &'static str) -> String {
    if selected == crate::route::Quality::Auto {
        format!("{absent} · controller idle")
    } else {
        "inactive · manual quality".to_string()
    }
}

/// The last request's observed transfer rate.  It is deliberately NOT called connection speed:
/// an HLS object can only deliver the bytes it contains, so the observation is censored by current
/// demand rather than being an independent speed test to the server.
fn abr_link(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    if d.abr_mode == 0 {
        return inactive_model(selected, "not sampled");
    }
    if d.abr_net_kbps < 0 {
        return "waiting for first measurement".to_string();
    }
    let mut s = format!("{} · stream-limited", abr_rate(d.abr_net_kbps));
    if d.abr_unc_pm >= 0 {
        s.push_str(&format!(" · ±{}%", d.abr_unc_pm / 10));
    }
    if d.abr_samples >= 0 {
        s.push_str(&format!(" · n={}", d.abr_samples));
    }
    s
}

/// What the decision may spend, beside what the current delivery demands.  Keeping it separate
/// from [`abr_link`] prevents a capped object sample from masquerading as physical link capacity.
fn abr_budget(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    if d.abr_mode == 0 {
        return inactive_model(selected, "not computed");
    }
    if d.abr_safe_kbps < 0 {
        return "waiting for conservative budget".to_string();
    }
    let demand = if d.abr_mode == crate::player::ABR_MODE_HLS {
        d.abr_media_kbps
    } else {
        d.abr_kbps
    };
    if demand <= 0 {
        return format!("{} budget · demand unknown", abr_rate(d.abr_safe_kbps));
    }
    let delta = d.abr_safe_kbps - demand;
    format!(
        "{} budget · {} measured stream · {} {}",
        abr_rate(d.abr_safe_kbps),
        abr_rate(demand),
        if delta >= 0 { "headroom" } else { "short" },
        abr_rate(delta.abs()),
    )
}

/// The OBSERVED buffer dynamics only.  The controller's conservative counterfactual horizon is a
/// different quantity and lives on [`abr_risk`]; mixing the two produced the photographed
/// `+0.2 s/s · starves in 116 s` contradiction.
fn observed_buffer_ms(d: &crate::player::Diag) -> Option<i64> {
    d.playable_buffer_ms
        .or_else(|| (d.abr_mode != 0 && d.abr_buffer_ms >= 0).then_some(d.abr_buffer_ms))
}

fn abr_buffer(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    let Some(buffer_ms) = observed_buffer_ms(d) else {
        return if d.abr_mode == 0 && selected == crate::route::Quality::Auto {
            "waiting for media timestamps · controller idle".to_string()
        } else {
            "waiting for media timestamps".to_string()
        };
    };
    if d.abr_mode == 0 {
        return if selected == crate::route::Quality::Auto {
            format!(
                "{:.1} s · observed reserve · controller idle",
                buffer_ms as f64 / 1_000.0
            )
        } else {
            format!("{:.1} s · observed reserve", buffer_ms as f64 / 1_000.0)
        };
    }
    let trend = match d.abr_slope_ms_per_s.cmp(&0) {
        std::cmp::Ordering::Greater => "filling",
        std::cmp::Ordering::Less => "draining",
        std::cmp::Ordering::Equal => "steady",
    };
    format!(
        "{:.1} s · {:+.2} s/s · {trend}",
        buffer_ms as f64 / 1_000.0,
        d.abr_slope_ms_per_s as f64 / 1_000.0,
    )
}

fn risk_percent(d: &crate::player::Diag) -> Option<i64> {
    (d.abr_risk >= 0)
        .then(|| d.abr_risk.saturating_mul(100) / i64::from(crate::abr::RISK_SCORE_MAX).max(1))
}

/// The discounted model's what-if horizon, explicitly labelled as such.  A finite value is not a
/// fault by itself: with a filling observed buffer it is evidence about uncertainty, not a claim
/// that playback is currently draining.
fn abr_risk(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    if d.abr_mode == 0 {
        return inactive_model(selected, "not computed");
    }
    let mut s = match d.abr_starve_secs {
        n if n >= 0 => format!("conservative horizon {n} s"),
        _ => "no conservative deficit".to_string(),
    };
    if let Some(pct) = risk_percent(d) {
        s.push_str(&format!(" · risk {pct}%"));
    }
    s
}

/// End-to-end acquisition wall time per media duration. It spans request open, PMS wait, pacing
/// and path transfer, so it is useful cadence telemetry but cannot identify an independent
/// encoder or server-load constraint. Above 1.0x means this operating point loses reserve over
/// that complete service episode; it does not say which component caused the loss.
///
/// `predicted` projects the same total-acquisition observation through the candidate's calibrated
/// work class for diagnostics only. Neither number is charged as a second admission gate.
fn abr_acquisition_cadence(d: &crate::player::Diag) -> String {
    if !crate::route::is_transcoding() {
        return "not sampled · direct play".to_string();
    }
    if crate::route::is_remux() {
        return "not sampled · stream copy".to_string();
    }
    if d.abr_ratio_pm < 0 {
        return "active · timing not sampled".to_string();
    }
    let mut s = format!("{:.2}x measured", d.abr_ratio_pm as f64 / 1_000.0);
    if d.abr_pred_pm >= 0 {
        s.push_str(&format!(
            " · {:.2}x predicted",
            d.abr_pred_pm as f64 / 1_000.0
        ));
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
    let Ok(kbps) = u32::try_from(kbps) else {
        return "unknown raster";
    };
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

/// Demand is what the current delivery has to carry, not the request ceiling when PMS emitted a
/// smaller VBR stream. Manual fixed HLS has no segment estimator, so its explicit ceiling is the
/// honest planning demand; Original uses the whole-file transport rate captured by the resolve.
fn chart_demand_kbps(d: &crate::player::Diag, selected: crate::route::Quality) -> i64 {
    match d.abr_mode {
        crate::player::ABR_MODE_HLS => [d.abr_media_kbps, d.abr_declared_kbps, d.abr_kbps]
            .into_iter()
            .find(|&v| v > 0)
            .unwrap_or(-1),
        crate::player::ABR_MODE_ORIGINAL if d.abr_kbps > 0 => d.abr_kbps,
        _ if selected == crate::route::Quality::Original && d.source_kbps > 0 => d.source_kbps,
        _ => selected.ceiling().map_or(-1, |c| i64::from(c.max_kbps)),
    }
}

/// Bytes delivered to FFmpeg during this held sample, expressed as a wire rate. A counter reset is
/// a reload inside the interval: the new counter still names bytes received since that reset and
/// is therefore the best lower bound available. `-1` keeps "not observed" distinct from an idle
/// network that genuinely delivered zero bytes.
fn network_activity_kbps(net_rx: i64, prev_net_rx: i64, prev_at: u32, now: u32) -> i64 {
    if prev_at == 0 || now <= prev_at || net_rx < 0 {
        return -1;
    }
    let bytes = if net_rx >= prev_net_rx {
        net_rx - prev_net_rx
    } else {
        net_rx.max(0)
    };
    let dt_ms = i64::from(now - prev_at).max(1);
    bytes.saturating_mul(8) / dt_ms
}

fn chart_rate(kbps: i64) -> String {
    match kbps {
        ..=-1 => "—".to_string(),
        0 => "0 kbps".to_string(),
        _ => abr_rate(kbps),
    }
}

fn chart_budget_demand(budget_kbps: i64, demand_kbps: i64) -> String {
    if budget_kbps.max(demand_kbps) >= 1_000 {
        let n = |v: i64| {
            if v < 0 {
                "—".to_string()
            } else {
                format!("{:.1}", v as f64 / 1_000.0)
            }
        };
        format!("{} budget · {} demand Mbps", n(budget_kbps), n(demand_kbps))
    } else {
        let n = |v: i64| {
            if v < 0 {
                "—".to_string()
            } else {
                v.to_string()
            }
        };
        format!("{} budget · {} demand kbps", n(budget_kbps), n(demand_kbps))
    }
}

fn chart_values(history: &SweepHistory) -> [String; 3] {
    let Some(s) = history.latest() else {
        return ["—".into(), "—".into(), "—".into()];
    };
    [
        chart_budget_demand(s.budget_kbps, s.demand_kbps),
        format!(
            "{} · mean {}",
            chart_rate(s.activity_kbps),
            history
                .mean_activity_kbps()
                .map(chart_rate)
                .unwrap_or_else(|| "—".to_string()),
        ),
        if s.buffer_ms >= 0 {
            format!("{:.1} s", s.buffer_ms as f64 / 1_000.0)
        } else {
            "—".to_string()
        },
    ]
}

fn chart_key_width_for(widest_label_px: f32) -> f32 {
    widest_label_px.max(CHART_LABEL_FALLBACK_PX) + theme::space::SM
}

/// Width of the chart's left label gutter. [`Label`] deliberately lets glyph ink overflow its
/// frame, so the plot cannot use a guessed frame width as its origin: measure the actual ink and
/// then add the design system's ordinary sibling gap.
fn chart_key_width() -> f32 {
    let cached = unsafe { addr_of_mut!(CHART_KEY_W).read() };
    if cached > 0.0 {
        return cached;
    }
    let measured = CHART_LABELS
        .iter()
        .filter_map(|label| CString::new(*label).ok())
        .map(|label| crate::text::text_width(label.as_ptr(), theme::size::DIAGNOSTIC, 0))
        .fold(0.0f32, f32::max);
    let width = chart_key_width_for(measured);
    // `text_width` is zero before text initialisation. Use the conservative width for this frame,
    // but retry rather than permanently caching an absence that existed only during boot.
    if measured > 0.0 {
        unsafe { addr_of_mut!(CHART_KEY_W).write(width) };
    }
    width
}

fn decoded_raster(d: &crate::player::Diag) -> String {
    match (d.video_w, d.video_h) {
        (w, h) if w > 0 && h > 0 => format!("{w}×{h}"),
        _ => "awaiting frames".to_string(),
    }
}

/// Keep the three different meanings that used to be collapsed into "HLS 22 Mbps / 4K" on one
/// line: what the client requested as a ceiling, what PMS says it produced, and what the decoder
/// actually opened.  The controller's next choice belongs in Action; repeating it here made the
/// widest real row wrap and therefore made every row below it move between photographs.
fn abr_quality(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    match d.abr_mode {
        crate::player::ABR_MODE_ORIGINAL => format!(
            "Original · source {} · decoded {}",
            abr_rate(d.abr_kbps),
            decoded_raster(d),
        ),
        crate::player::ABR_MODE_HLS => {
            let mut now = format!(
                "request ≤{} / ≤{}",
                abr_rate(d.abr_kbps),
                abr_raster(d.abr_kbps),
            );
            if d.abr_declared_kbps > 0 {
                now.push_str(&format!(" · PMS {}", abr_rate(d.abr_declared_kbps)));
            }
            now.push_str(&format!(" · decoded {}", decoded_raster(d)));
            now
        }
        _ => format!("{} · decoded {}", selected.label(), decoded_raster(d)),
    }
}

fn abr_action(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    if d.abr_mode == 0 {
        return if selected == crate::route::Quality::Auto {
            "none · controller idle".to_string()
        } else {
            "fixed by user".to_string()
        };
    }
    if d.abr_mode == crate::player::ABR_MODE_ORIGINAL {
        // **How long has this been going on**, in seconds of WALL clock. It printed a count of
        // measurement windows, and a window was 750 ms of ACTIVE BODY-READ time — a clock that
        // stops under backpressure, i.e. exactly when the buffer is healthy — so "3 windows" named
        // durations an order of magnitude apart and a viewer could not tell which (N13).
        //
        // There is still no denominator, for the reason there was not one before: a fallback is
        // decided by the horizon and by utility, not by reaching a fixed number, and a fraction
        // would imply a countdown that is a lie in both directions — an imminent starvation acts on
        // the FIRST window, and a shortfall with a deep reserve never acts at all.
        return match d.abr_unsafe_deficit_ms {
            ms if ms <= 0 => "watching Original".to_string(),
            ms => format!(
                "collecting fallback evidence · {}",
                crate::ui::fmt::secs_short(ms)
            ),
        };
    }
    let target = abr_rate(d.abr_target_kbps);
    let action = match d.abr_action {
        crate::player::ABR_ACTION_STEADY
            if d.abr_optimal_kbps > 0 && d.abr_optimal_kbps != d.abr_kbps =>
        {
            format!("hold · model asks {}", abr_rate(d.abr_optimal_kbps))
        }
        crate::player::ABR_ACTION_STEADY => "hold current".to_string(),
        crate::player::ABR_ACTION_PRIME_DOWN => format!("priming down to {target}"),
        crate::player::ABR_ACTION_PRIME_UP => format!("priming up to {target}"),
        crate::player::ABR_ACTION_COMMIT_DOWN => format!("changed down to {target}"),
        crate::player::ABR_ACTION_COMMIT_UP => format!("changed up to {target}"),
        crate::player::ABR_ACTION_REJECT_DOWN => format!("hold · rejected {target}"),
        crate::player::ABR_ACTION_REJECT_UP => format!("hold · rejected {target}"),
        crate::player::ABR_ACTION_PROBE_ORIGINAL => "checking Original link".to_string(),
        crate::player::ABR_ACTION_RECOVER_ORIGINAL => "switching back to Original".to_string(),
        crate::player::ABR_ACTION_ORIGINAL_PROBE_FAILED => format!(
            "hold · Original check failed{}",
            if d.abr_failure_status > 0 {
                format!(" · HTTP {}", d.abr_failure_status)
            } else {
                String::new()
            },
        ),
        crate::player::ABR_ACTION_PRIME_REFRESH => format!("refreshing request {target}"),
        crate::player::ABR_ACTION_COMMIT_REFRESH => format!("refreshed request {target}"),
        crate::player::ABR_ACTION_REJECT_REFRESH => {
            "hold · refreshed response unchanged".to_string()
        }
        _ => "starting".to_string(),
    };
    action
}

fn abr_reason(d: &crate::player::Diag, selected: crate::route::Quality) -> String {
    // A typed source failure is playback evidence, not an HLS-only controller field. Keep it
    // visible through rollback, a manual pin and the short controller-restart interval; otherwise
    // the exact HTTP status disappears behind the generic "no adaptive session" sentence.
    if let Some(reason) = original_failure_reason(d) {
        return reason;
    }
    if d.abr_mode == 0 {
        return if selected == crate::route::Quality::Auto {
            "no adaptive session".to_string()
        } else {
            "adaptive controller inactive".to_string()
        };
    }
    if d.abr_mode == crate::player::ABR_MODE_ORIGINAL {
        return if d.abr_unsafe_deficit_ms <= 0 {
            "sample sustains source demand".to_string()
        } else {
            "sample below source demand".to_string()
        };
    }
    abr_why_text(d.abr_why)
        .unwrap_or("waiting for decision")
        .to_string()
}

fn original_failure_reason(d: &crate::player::Diag) -> Option<String> {
    let status = d.abr_failure_status;
    match d.abr_failure_kind {
        crate::player::ABR_FAILURE_ORIGINAL_HTTP => Some(match status {
            503 | 509 => format!("PMS refused Original source · HTTP {status}"),
            500..=599 => format!("PMS failed Original source · HTTP {status}"),
            n if n > 0 => format!("PMS rejected Original source · HTTP {n}"),
            _ => "PMS rejected Original source".to_string(),
        }),
        crate::player::ABR_FAILURE_ORIGINAL_DEADLINE => {
            Some("Original source probe timed out".to_string())
        }
        crate::player::ABR_FAILURE_ORIGINAL_TRANSPORT => {
            Some("Original source connection failed".to_string())
        }
        crate::player::ABR_FAILURE_ORIGINAL_NO_BODY => {
            Some("Original source returned no body".to_string())
        }
        crate::player::ABR_FAILURE_ORIGINAL_OPEN => Some(if status > 0 {
            format!("Original stream failed after HTTP {status}")
        } else {
            "Original stream could not be opened".to_string()
        }),
        _ => None,
    }
}

/// The reason code in words a reader who has never seen this codebase can act on. `None` for
/// "nothing has decided yet", which is a real state at the top of a playback rather than a fault.
fn abr_why_text(why: u8) -> Option<&'static str> {
    match why {
        crate::player::ABR_WHY_SAFE_BUDGET => Some("link has room"),
        crate::player::ABR_WHY_UNSAFE_STATE => Some("current stream losing reserve"),
        crate::player::ABR_WHY_PRODUCTION => Some("acquisition behind"),
        crate::player::ABR_WHY_BUFFER => Some("reserve low"),
        // Deliberately not phrased as a constraint like the codes above: this is the state where
        // the controller has nothing left to try, and a reader watching the picture stop is owed
        // that rather than a fifth thing that sounds like a knob.
        crate::player::ABR_WHY_LADDER_FLOOR => Some("lowest quality"),
        // The one code that names a DEADLINE rather than a conservation deficit. "current stream
        // losing reserve" says the completed delivery bag costs more wall time than media it
        // supplies; this says the reserve is now too short to reach the next credit at all.
        crate::player::ABR_WHY_STARVATION => Some("buffer running out"),
        crate::player::ABR_WHY_REJECT_BACKOFF => Some("HLS candidate failed · retry pending"),
        // Phrased for the person holding the phone, not for the controller: what they can see is
        // that the picture is not improving, and these three are the three different reasons.
        crate::player::ABR_WHY_NO_TARGET => Some("no better quality fits"),
        crate::player::ABR_WHY_EVIDENCE => Some("still measuring"),
        crate::player::ABR_WHY_AT_BEST => Some("no higher request in ladder"),
        crate::player::ABR_WHY_RESERVE_UNKNOWN => Some("waiting for audio"),
        crate::player::ABR_WHY_DEADLINE_ROLLBACK => Some("fetch deadline rollback"),
        crate::player::ABR_WHY_RESPONSE_LIMITED => Some("PMS output below request"),
        // The one code here whose constraint is the VIEWER rather than the link or the server, so
        // it says so: the picture could improve and the improvement is not worth another visible
        // change this soon after the last one.
        crate::player::ABR_WHY_SWITCH_COST => Some("holding after a recent quality change"),
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

/// AUs/second per lane since the previous sample, used by the instantaneous Fed row.
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
    let src = if src.is_empty() {
        "—".to_string()
    } else {
        src
    };
    let sent = if sent.is_empty() {
        "—".to_string()
    } else {
        sent
    };
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

/// The PMS release triplet ("1.43.3.10861-cd85035e7" → "1.43.3"). The build/hash carry no support
/// signal the release does not, while the shorter stable form keeps the Connection row directly
/// comparable and leaves its Pass verdict visible. The event log retains the full string.
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
const COL_GAP: f32 = 32.0;
/// Title, build/firmware, playback verdict and the two column headings — each on its own cap band.
const HEAD_H: f32 = 118.0;
const PANEL_W: f32 = 2.0 * FIELD_COL_W + COL_GAP + 2.0 * PAD;

/// The panel's box, SIZED TO ITS CONTENT rather than to the screen.
///
/// Two consequences, and both REMOVE code rather than adding it. The video stays visible around it,
/// which is the point of a stats overlay you watch playback under — the first version was a
/// full-screen opaque card that made "is anything on screen?" unanswerable while the panel was up.
/// And it sits entirely ABOVE the transport (`player_hud::CTRL_Y`), so a pointer click can never
/// land on the scrubber's rects THROUGH an opaque card — which was the only reason the click path
/// needed a close-on-click arm at all.
pub(crate) fn panel_rect() -> Rect {
    let idle = unsafe { addr_of_mut!(IDLE).read() };
    let (w, h) = if idle {
        // Before playback there is no delivery history to chart. Keep the support card compact and
        // price exactly the device rows sampled for this frame.
        let rows = unsafe { &*addr_of_mut!(ROWS) };
        let lines = FieldList::wrapped_line_count(rows);
        (
            FIELD_COL_W + 2.0 * PAD,
            HEAD_H + FieldList::height(lines) + PAD,
        )
    } else {
        // Playback keeps fixed comparable columns. The chart is four row pitches under the
        // shorter model column, so both sides share one measured height without clipping.
        let cols = unsafe { &*addr_of_mut!(COLUMNS) };
        let left = FieldList::wrapped_line_count(&cols[0]).max(LEFT_ROWS);
        let right = FieldList::wrapped_line_count(&cols[1]).max(RIGHT_ROWS) + CHART_ROWS;
        (PANEL_W, HEAD_H + FieldList::height(left.max(right)) + PAD)
    };
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
        Label::new(cs.as_ptr(), theme::size::BODY, theme::TEXT_PRIMARY)
            .bold()
            .draw(p, Rect::new(inner, frame.y + 10.0, iw, 30.0));
    }
    // Build + firmware as ONE identity line. Two lines was one more than the fact needs.
    if let Ok(cs) = CString::new(head[0].as_str()) {
        Label::new(cs.as_ptr(), theme::size::DIAGNOSTIC, theme::TEXT_TERTIARY)
            .draw(p, Rect::new(inner, frame.y + 38.0, iw, 24.0));
    }
    // the verdict — the one line that says what the pipeline thinks it is doing
    if let Ok(cs) = CString::new(head[1].as_str()) {
        let ink = if head[1].starts_with("Playback error") {
            theme::DANGER
        } else {
            theme::TEXT_PRIMARY
        };
        Label::new(cs.as_ptr(), theme::size::DIAGNOSTIC, ink)
            .bold()
            .draw(p, Rect::new(inner, frame.y + 62.0, iw, 24.0));
    }

    let top = frame.y + HEAD_H;
    if unsafe { addr_of_mut!(IDLE).read() } {
        if let Ok(cs) = CString::new("DEVICE / SERVER") {
            Label::new(cs.as_ptr(), theme::size::DIAGNOSTIC, theme::TEXT_SECONDARY)
                .bold()
                .draw(p, Rect::new(inner, frame.y + 90.0, FIELD_COL_W, 24.0));
        }
        let rows = unsafe { &*addr_of_mut!(ROWS) };
        FieldList::new(
            rows,
            Rect::new(inner, top, FIELD_COL_W, frame.h - HEAD_H - PAD),
        )
        .draw(&e, p);
        return;
    }

    let right_x = inner + FIELD_COL_W + COL_GAP;
    for (title, x) in [("STREAM / OUTPUT", inner), ("DELIVERY / CONTROL", right_x)] {
        if let Ok(cs) = CString::new(title) {
            Label::new(cs.as_ptr(), theme::size::DIAGNOSTIC, theme::TEXT_SECONDARY)
                .bold()
                .draw(p, Rect::new(x, frame.y + 90.0, FIELD_COL_W, 24.0));
        }
    }

    let cols = unsafe { &*addr_of_mut!(COLUMNS) };
    FieldList::new(
        &cols[0],
        Rect::new(inner, top, FIELD_COL_W, frame.h - HEAD_H - PAD),
    )
    .draw(&e, p);
    FieldList::new(
        &cols[1],
        Rect::new(right_x, top, FIELD_COL_W, frame.h - HEAD_H - PAD),
    )
    .draw(&e, p);

    let cy = top + FieldList::height(FieldList::wrapped_line_count(&cols[1]).max(RIGHT_ROWS));
    draw_chart(
        p,
        Rect::new(
            right_x,
            cy,
            FIELD_COL_W,
            (frame.y + frame.h - PAD - cy).max(0.0),
        ),
    );
}

/// Three fixed-slot sweep lanes, deliberately named after quantities the app actually knows:
/// conservative delivery BUDGET against current media DEMAND, bytes delivered to FFmpeg during
/// this sample, and the player's content-time reserve. It is not labelled connection speed —
/// an object can only send the bytes it contains, so ordinary playback does not independently
/// identify the physical link ceiling.
///
/// Deliberately LOCAL rather than a `ui::widgets` component. It is a debug read-out, not a design
/// system piece: it has no focus, no state and no springs, and
/// promoting it would put a diagnostic-only shape in the shared vocabulary for one caller.
fn draw_chart(p: Painter, r: Rect) {
    if r.h < 60.0 {
        return;
    }
    let history = unsafe { addr_of_mut!(HISTORY).read() };
    let values = unsafe { &*addr_of_mut!(CHART_VALUES) };
    let peak = |pick: fn(SweepSample) -> i64| {
        history
            .slots
            .iter()
            .copied()
            .filter(|s| s.valid)
            .map(pick)
            .max()
            .unwrap_or(0)
            .max(1) as f32
    };
    let link_peak = history
        .slots
        .iter()
        .copied()
        .filter(|s| s.valid)
        .flat_map(|s| [s.budget_kbps, s.demand_kbps])
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let activity_peak = peak(|s| s.activity_kbps);
    let activity_mean = history.mean_activity_kbps();
    let buffer_peak = peak(|s| s.buffer_ms);

    const CHART_VALUE_W: f32 = 280.0;
    const CHART_GAP: f32 = 8.0;
    let lane_h = r.h / 3.0;
    let key_w = chart_key_width();
    let bx = r.x + key_w;
    let bars_w = (r.w - key_w - CHART_VALUE_W - CHART_GAP).max(1.0);
    let value_x = bx + bars_w + CHART_GAP;
    let bw = bars_w / HIST_N as f32;

    for (i, label) in CHART_LABELS.into_iter().enumerate() {
        let by = r.y + i as f32 * lane_h;
        let band = (lane_h - 4.0).max(8.0);
        if let Ok(cs) = CString::new(label) {
            Label::new(cs.as_ptr(), theme::size::DIAGNOSTIC, theme::TEXT_TERTIARY)
                .draw(p, Rect::new(r.x, by, key_w, lane_h));
        }
        let latest = history.latest().unwrap_or(SweepSample::EMPTY);
        let value_tint = match i {
            0 => match link_state(latest.budget_kbps, latest.demand_kbps) {
                LinkState::Sustains => theme::DIAG_LINK_SUSTAINS,
                LinkState::Deficit => theme::DIAG_LINK_DEFICIT,
                LinkState::Unknown => theme::TEXT_TERTIARY,
            },
            1 => theme::DIAG_NETWORK_ACTIVITY,
            _ => theme::RESUME_FILL,
        };
        if let Ok(cs) = CString::new(values[i].as_str()) {
            Label::new(cs.as_ptr(), theme::size::DIAGNOSTIC, value_tint)
                .draw(p, Rect::new(value_x, by, CHART_VALUE_W, lane_h));
        }
        // The ground makes a genuine run of zero activity legible as data rather than blank card.
        p.rect(
            Rect::new(bx, by + band - 1.0, bars_w, 1.0),
            0.0,
            theme::TEXT_TERTIARY,
            theme::TEXT_TERTIARY,
            0.0,
        );
        for k in 0..HIST_N {
            // `k`, not `(head + k) % N`: slots are physical x coordinates and never rotate.
            let s = history.slot(k);
            if !s.valid {
                continue;
            }
            let (raw, scale, tint) = match i {
                0 => (
                    s.budget_kbps,
                    link_peak,
                    match link_state(s.budget_kbps, s.demand_kbps) {
                        LinkState::Sustains => theme::DIAG_LINK_SUSTAINS,
                        LinkState::Deficit => theme::DIAG_LINK_DEFICIT,
                        LinkState::Unknown => theme::TEXT_TERTIARY,
                    },
                ),
                1 => (s.activity_kbps, activity_peak, theme::DIAG_NETWORK_ACTIVITY),
                _ => (s.buffer_ms, buffer_peak, theme::RESUME_FILL),
            };
            if raw >= 0 {
                let bh = ((raw as f32 / scale).clamp(0.0, 1.0) * band).max(0.0);
                if bh >= 1.0 {
                    p.rect(
                        Rect::new(bx + k as f32 * bw, by + band - bh, (bw - 1.0).max(1.0), bh),
                        0.0,
                        tint,
                        tint,
                        0.0,
                    );
                }
            }
            // Demand is a per-sample threshold, not a second filled series. A one-pixel amber
            // marker keeps "budget is high" visually distinct from "budget covers THIS stream".
            if i == 0 && s.demand_kbps > 0 {
                let y = by + band - (s.demand_kbps as f32 / link_peak).clamp(0.0, 1.0) * band;
                p.rect(
                    Rect::new(bx + k as f32 * bw, y, (bw - 1.0).max(1.0), 1.0),
                    0.0,
                    theme::RESUME_FILL,
                    theme::RESUME_FILL,
                    0.0,
                );
            }
        }
        // One line over the same exact 16-second physical window as the bars. It is the arithmetic
        // mean inflow, explicitly labelled at the right; unlike a percentile it makes no claim
        // about unobserved link capacity and unlike the bursty median it does not collapse to zero.
        if i == 1 {
            if let Some(mean_kbps) = activity_mean {
                let y = by + band - (mean_kbps as f32 / activity_peak).clamp(0.0, 1.0) * band;
                p.rect(
                    Rect::new(bx, y, bars_w, 2.0),
                    0.0,
                    theme::DIAG_NETWORK_MEAN,
                    theme::DIAG_NETWORK_MEAN,
                    0.0,
                );
            }
        }
    }
    // One cursor across all lanes: the next cell to be overwritten. It advances at the panel's
    // 2 Hz sample cadence; there is deliberately no per-frame animation clock.
    let cursor_x = bx + history.head() as f32 * bw;
    p.rect(
        Rect::new(cursor_x, r.y, 2.0, r.h),
        0.0,
        theme::DIAG_SWEEP_CURSOR,
        theme::DIAG_SWEEP_CURSOR,
        0.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::{SCR_H, SCR_W};

    /// The YouTube-style plot is a sweep, not a scrolling queue: advancing time overwrites the
    /// next physical cell and moves the cursor, while every other cell stays at the same x. A
    /// phone photo can therefore compare two runs without the whole shape moving underneath it.
    #[test]
    fn chart_history_overwrites_fixed_slots_and_moves_only_its_cursor() {
        let mut history = SweepHistory::new();
        for n in 0..HIST_N + 2 {
            history.push(SweepSample {
                activity_kbps: n as i64,
                ..SweepSample::EMPTY
            });
        }

        assert_eq!(history.head(), 2, "the cursor is the next physical slot");
        assert_eq!(history.slot(0).activity_kbps, HIST_N as i64);
        assert_eq!(history.slot(1).activity_kbps, HIST_N as i64 + 1);
        assert_eq!(
            history.slot(2).activity_kbps,
            2,
            "untouched cells do not rotate left"
        );
    }

    /// `Label` frames do not clip their ink. The widest chart caption in the shipped diagnostic
    /// face measures about 200 px, so the plot must begin after that ink plus the standard sibling
    /// gap. The old 178 px gutter put every plot underneath its own caption.
    #[test]
    fn chart_plot_starts_after_the_label_ink() {
        const WIDEST_LABEL_PX: f32 = 200.0;
        let key_w = chart_key_width_for(WIDEST_LABEL_PX);
        assert!(
            key_w >= WIDEST_LABEL_PX + theme::space::SM,
            "plot begins at {key_w}px, inside the label's {}px paint + {}px gap",
            WIDEST_LABEL_PX,
            theme::space::SM,
        );
    }

    /// There is no taste threshold in the link colour. Green means the conservative budget
    /// physically covers demand; red means it does not; absent evidence stays neutral.
    #[test]
    fn chart_link_colour_is_the_budget_inequality() {
        assert_eq!(link_state(-1, 4_000), LinkState::Unknown);
        assert_eq!(link_state(4_000, -1), LinkState::Unknown);
        assert_eq!(link_state(3_999, 4_000), LinkState::Deficit);
        assert_eq!(link_state(4_000, 4_000), LinkState::Sustains);
        assert_eq!(link_state(12_000, 4_000), LinkState::Sustains);
    }

    #[test]
    fn network_activity_is_bytes_delivered_during_the_sample() {
        assert_eq!(
            network_activity_kbps(1_000_000, 0, 0, 500),
            -1,
            "first sample has no interval"
        );
        // 1 MB over 500 ms = 16,000 kbit/s (decimal wire-rate units).
        assert_eq!(
            network_activity_kbps(2_000_000, 1_000_000, 500, 1_000),
            16_000
        );
        // A reload resets the cumulative counter; the new counter is still the bytes observed in
        // this interval, rather than a negative spike or a missing bar.
        assert_eq!(
            network_activity_kbps(500_000, 2_000_000, 1_000, 1_500),
            8_000
        );
        assert_eq!(
            chart_rate(0),
            "0 kbps",
            "known idle is not an unknown measurement"
        );
    }

    #[test]
    fn network_mean_includes_idle_intervals_and_excludes_unknown_ones() {
        let mut history = SweepHistory::new();
        for activity_kbps in [-1, 0, 100, 200] {
            history.push(SweepSample {
                activity_kbps,
                ..SweepSample::EMPTY
            });
        }
        assert_eq!(history.mean_activity_kbps(), Some(100));
        assert_eq!(
            chart_values(&history)[1],
            "200 kbps · mean 100 kbps",
            "the right-hand legend names both the current bar and the exact visible-window statistic",
        );
    }

    #[test]
    fn manual_original_keeps_the_same_demand_lane() {
        let d = crate::player::Diag {
            source_kbps: 28_000,
            ..Default::default()
        };
        assert_eq!(
            chart_demand_kbps(&d, crate::route::Quality::Original),
            28_000
        );
        let hls = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_kbps: 22_000,
            abr_declared_kbps: 18_600,
            abr_media_kbps: 17_900,
            ..Default::default()
        };
        assert_eq!(
            chart_demand_kbps(&hls, crate::route::Quality::Auto),
            17_900,
            "actual measured HLS demand outranks both PMS declaration and request ceiling",
        );
    }

    /// The row budgets are the design: mode changes replace values, never geometry.
    #[test]
    fn the_read_out_never_outgrows_its_budget() {
        // Every video-plane shape and every Auto shape: the exported path states more about the
        // plane than ACB does, and Auto trades the FFmpeg row for its five model rows — neither
        // may change the budget.
        for vp in [
            crate::player::VP_ACB,
            crate::player::VP_EXPORTED,
            crate::player::VP_NONE,
        ] {
            for abr_mode in [
                0,
                crate::player::ABR_MODE_ORIGINAL,
                crate::player::ABR_MODE_HLS,
            ] {
                let d = crate::player::Diag {
                    vp_mode: vp,
                    abr_mode,
                    ..Default::default()
                };
                let [left, right] = columns(&d, (0, 0, 0), 1_000);
                assert_eq!(left.len(), LEFT_ROWS, "vp={vp} abr={abr_mode}: left");
                assert_eq!(right.len(), RIGHT_ROWS, "vp={vp} abr={abr_mode}: right");
            }
        }
    }

    /// A photograph must be directly comparable across every delivery mode.  Rows that disappear
    /// in fixed/Original mode make the reader compare two different instruments and, worse, move
    /// every pipeline fact to a different y between photographs.
    #[test]
    fn every_delivery_mode_keeps_the_same_schema() {
        let schema = |mode| {
            rows(
                &crate::player::Diag {
                    abr_mode: mode,
                    ..Default::default()
                },
                (0, 0, 0),
                1_000,
            )
            .into_iter()
            .map(|f| f.key)
            .collect::<Vec<_>>()
        };
        let fixed = schema(0);
        assert!(
            fixed.contains(&"Acquisition"),
            "the cadence row disappeared from the fixed schema"
        );
        assert!(
            !fixed.contains(&"Encoder"),
            "the end-to-end cadence was mislabeled as server state"
        );
        assert_eq!(
            schema(crate::player::ABR_MODE_ORIGINAL),
            fixed,
            "Original moved or added rows"
        );
        assert_eq!(
            schema(crate::player::ABR_MODE_HLS),
            fixed,
            "HLS moved or added rows"
        );
    }

    /// Regression for the photographed `6.9 s · +0.2 s/s · starves in 116 s` reading.  The
    /// observed buffer is FILLING; 116 s is a separate conservative what-if horizon computed from
    /// the discounted budget.  Calling it starvation and tinting the Buffer row red states a
    /// prediction the controller did not make.
    #[test]
    fn a_conservative_horizon_does_not_overrule_an_observed_filling_buffer() {
        let d = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_kbps: 320,
            abr_safe_kbps: 301,
            abr_net_kbps: 499,
            abr_buffer_ms: 6_918,
            abr_slope_ms_per_s: 233,
            abr_starve_secs: 116,
            abr_risk: 4,
            ..Default::default()
        };
        let mut fields = Vec::new();
        abr_rows(&d, crate::route::Quality::Auto, &mut fields);
        let buffer = fields
            .iter()
            .find(|f| f.key == "Buffer")
            .expect("Buffer row");
        let value = buffer.val.as_deref().unwrap_or_default();
        assert!(
            value.contains("filling"),
            "observed trend is missing: {value}"
        );
        assert!(
            !value.contains("starves"),
            "a conservative horizon was labelled as fact: {value}"
        );
        assert_ne!(
            buffer.tone,
            crate::ui::widgets::Tone::Fault,
            "a filling buffer is not a fault"
        );
        let risk = fields
            .iter()
            .find(|f| f.key == "Risk")
            .expect("separate Risk row");
        assert!(
            risk.val
                .as_deref()
                .unwrap_or_default()
                .contains("conservative horizon 116 s"),
            "the what-if horizon still has to remain visible"
        );
    }

    #[test]
    fn manual_quality_keeps_the_same_observed_buffer_instrument() {
        let d = crate::player::Diag {
            playable_buffer_ms: Some(12_345),
            ..Default::default()
        };
        assert_eq!(
            abr_buffer(&d, crate::route::Quality::Original),
            "12.3 s · observed reserve",
        );
        assert_eq!(
            abr_buffer(&d, crate::route::Quality::P720),
            "12.3 s · observed reserve",
        );
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
            let Some(val) = f.val.as_deref() else {
                continue;
            };
            let lines = crate::ui::widgets::value_lines(val, FIELD_COL_W);
            assert_eq!(
                lines.len(),
                1,
                "`{}` wraps to {} lines: {val}",
                f.key,
                lines.len()
            );
        }
    }

    /// **The predicate that chooses the two lists**, and the case it has to get right is the one
    /// that is NOT obvious: a playback that finished, or that failed and was cleared, is `Idle`
    /// too — and there the pipeline block is precisely what a reader wants.
    #[test]
    fn only_a_session_that_never_loaded_gets_the_device_block() {
        use crate::player::PlaybackState as S;
        let fresh = crate::player::Diag::default();
        assert!(never_played(&fresh, S::Idle), "boot, nothing asked to play");
        // Every non-idle state keeps the pipeline block, including the two that precede a Load and
        // so still have `load_at == 0` — those are where "Load waiting · no connection" is the
        // answer, not a decoy.
        for st in [
            S::Resolving,
            S::Connecting,
            S::Buffering,
            S::Seeking,
            S::Playing,
            S::Error,
        ] {
            assert!(!never_played(&fresh, st), "{st:?} is a playback attempt");
        }
        // …and back at Idle after something HAS played, the post-mortem is the point.
        let played = crate::player::Diag {
            load_at: 4_200,
            ..Default::default()
        };
        assert!(
            !never_played(&played, S::Idle),
            "a finished session keeps its pipeline rows"
        );
    }

    /// The device block obeys the same one-line rule as the pipeline block, and says what it does
    /// NOT know rather than inventing it — `webos`/`devcaps` are unprobed under `cargo test`, so
    /// this runs in exactly the all-empty state a set whose nyx and codec table are unreadable
    /// would produce.
    #[test]
    fn the_device_block_fits_and_admits_what_it_could_not_read() {
        let v = device_rows();
        assert!(v.len() <= PANEL_ROWS, "{} rows", v.len());
        for f in &v {
            let Some(val) = f.val.as_deref() else {
                continue;
            };
            let lines = crate::ui::widgets::value_lines(val, FIELD_COL_W);
            assert_eq!(
                lines.len(),
                1,
                "`{}` wraps to {} lines: {val}",
                f.key,
                lines.len()
            );
        }
        let val = |key: &str| {
            v.iter()
                .find(|f| f.key == key)
                .and_then(|f| f.val.clone())
                .unwrap_or_default()
        };
        // Unprobed is UNKNOWN, not a plausible default — and it is a fault, because a report whose
        // set cannot be named is a report that cannot be acted on.
        assert!(val("Set").starts_with("unknown"), "Set = {}", val("Set"));
        assert!(
            v.iter()
                .find(|f| f.key == "Set")
                .map(|f| f.tone == crate::ui::widgets::Tone::Fault)
                .unwrap_or(false),
            "an unnamed set is a fault"
        );
        // THE POINT of `devcaps::measured`: with no table read, the row must not print this
        // project's dev-set profile as though it had measured the reporter's television.
        assert!(
            val("Decoder").contains("ASSUMED"),
            "Decoder = {}",
            val("Decoder")
        );
    }

    /// The device card is SHORTER and has no chart. Both come off one sampled flag, so the measure
    /// and the paint cannot disagree — the failure the two-rules version of this panel already had
    /// once, when reserved rows fell out of the bottom of the card onto the transport.
    #[test]
    fn the_device_card_drops_the_row_floor_and_the_chart() {
        let _g = crate::testlock::serial();
        unsafe {
            let saved_rows = addr_of_mut!(ROWS).replace(device_rows());
            let saved_idle = addr_of_mut!(IDLE).read();

            addr_of_mut!(IDLE).write(false);
            let pipeline_h = panel_rect().h;
            addr_of_mut!(IDLE).write(true);
            let device_h = panel_rect().h;

            drop(addr_of_mut!(ROWS).replace(saved_rows));
            addr_of_mut!(IDLE).write(saved_idle);

            assert!(
                device_h < pipeline_h,
                "device {device_h} not shorter than pipeline {pipeline_h}"
            );
            // Exactly the chart band plus the rows the floor would have reserved but the content
            // does not fill — no fudge factor, which is what makes this an assertion about the
            // layout rule rather than about a number somebody measured once.
            let n = FieldList::wrapped_line_count(&device_rows());
            let reserved = FieldList::height(RIGHT_ROWS) - FieldList::height(n);
            let chart_h = FieldList::height(CHART_ROWS);
            assert!((pipeline_h - device_h - reserved - chart_h).abs() < 0.5);
        }
    }

    /// A fresh, never-started session must read as faults, not as a healthy zero — that is the
    /// state a user photographs when nothing happens at all, and every row that can say "this did
    /// not happen" must say it.
    #[test]
    fn a_dead_session_marks_its_faults() {
        // `load_at` a full stall-window in the past: a Load that completed SECONDS ago with no
        // frame is the fault. The same session one tick after Load is NOT — see the test below.
        let d = crate::player::Diag {
            load_completed: true,
            load_at: 1_000,
            ..Default::default()
        };
        let v = rows(&d, (0, 0, 0), 1_000 + STALL_MS + 1);
        let faults: Vec<_> = v
            .iter()
            .filter(|f| f.tone == crate::ui::widgets::Tone::Fault)
            .map(|f| f.key)
            .collect();
        // One row per thing that did not happen: no video path, a Load with no callbacks, nothing
        // demuxed or fed, and no frame long after the Load completed.
        for expect in ["Picture", "Video plane", "Load", "Feed", "Frames"] {
            assert!(
                faults.contains(&expect),
                "{expect} should read as a fault; got {faults:?}"
            );
        }
    }

    /// The panel must sit entirely ABOVE the transport's control row. That is what makes a pointer
    /// click unambiguous — no part of the scrubber or the discs is ever underneath an opaque card —
    /// and it is why the click path needs no close-on-click arm.
    #[test]
    fn the_panel_clears_the_transport() {
        let bottom = MARGIN + HEAD_H + FieldList::height(LEFT_ROWS) + PAD;
        assert!(
            bottom < crate::ui::player_hud::CTRL_Y,
            "panel bottom {bottom} overlaps the control row at {}",
            crate::ui::player_hud::CTRL_Y
        );
        // through `panel_rect` itself, not a restatement of its arithmetic: its x is `MARGIN_X`
        // (the overscan side margin) while its y is `MARGIN`, and a copy here would have kept
        // spelling 60 for both.
        let p = panel_rect();
        assert!(
            p.x + p.w <= SCR_W - crate::ui::consts::MARGIN_X,
            "panel is wider than the safe frame"
        );
        assert!(
            crate::ui::consts::inside_safe(p),
            "the read-out is photographed — it must clear the overscan frame"
        );
    }

    /// **The chart used to be deletable by a green suite.** `draw_chart` returns silently below
    /// 60 px, and it was laid out in "whatever the right column had left" — so two extra rows
    /// anywhere took its slack, it stopped drawing, and nothing failed. Its band is RESERVED by
    /// `panel_rect` now; this grades that the reservation actually survives to the draw.
    #[test]
    fn the_chart_keeps_its_band_whatever_the_rows_do() {
        for vp in [
            crate::player::VP_ACB,
            crate::player::VP_EXPORTED,
            crate::player::VP_NONE,
        ] {
            for abr_mode in [
                0,
                crate::player::ABR_MODE_ORIGINAL,
                crate::player::ABR_MODE_HLS,
            ] {
                let d = crate::player::Diag {
                    vp_mode: vp,
                    abr_mode,
                    ..Default::default()
                };
                let [left, right] = columns(&d, (0, 0, 0), 1_000);
                let ll = FieldList::wrapped_line_count(&left).max(LEFT_ROWS);
                let rl = FieldList::wrapped_line_count(&right).max(RIGHT_ROWS);
                let h = HEAD_H + FieldList::height(ll.max(rl + CHART_ROWS)) + PAD;
                let band = h - PAD - HEAD_H - FieldList::height(rl);
                assert!(
                    band >= 60.0,
                    "vp={vp} abr={abr_mode}: chart band is {band}px, it would not draw"
                );
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
            v.iter()
                .find(|f| f.key == key)
                .and_then(|f| f.val.as_deref())
                .unwrap_or("missing")
                .to_string()
        };
        let build_with = |d: &crate::player::Diag, selected: crate::route::Quality| {
            let mut v = Vec::new();
            abr_rows(d, selected, &mut v);
            v
        };
        let build = |d: &crate::player::Diag| build_with(d, crate::route::Quality::Auto);

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
            abr_unsafe_deficit_ms: 1_500,
            video_w: 3_840,
            video_h: 2_160,
            ..Default::default()
        };
        let v = build(&original);
        assert_eq!(
            val(&v, "Quality"),
            "Original · source 11.4 Mbps · decoded 3840×2160"
        );
        assert_eq!(val(&v, "Sample"), "4.0 Mbps · stream-limited · ±18% · n=5");
        assert_eq!(
            val(&v, "Conservative"),
            "3.8 Mbps budget · 11.4 Mbps measured stream · short 7.6 Mbps",
        );
        assert_eq!(val(&v, "Buffer"), "2.8 s · -0.90 s/s · draining");
        assert_eq!(val(&v, "Risk"), "conservative horizon 3 s · risk 0%");
        // **Elapsed WALL time, with no denominator.** It read a count of measurement windows, and
        // a window was 750 ms of ACTIVE BODY-READ time — a clock that stops under backpressure,
        // i.e. exactly when the buffer is healthy — so "1 window" named durations an order of
        // magnitude apart and a viewer could not tell which (N13). Still no denominator, for the
        // reason there was not one before: a fallback is decided by the horizon and by utility,
        // and a fraction would promise a countdown that exists in neither direction.
        assert_eq!(val(&v, "Action"), "collecting fallback evidence · 1.5 s");
        assert_eq!(val(&v, "Reason"), "sample below source demand");
        // `Diag` is not `Copy` (it owns a `String`), and the Action row reads exactly two of its
        // fields, so a minimal probe states what the row depends on instead of cloning a struct of
        // thirty.
        let at = |ms: i64| {
            val(
                &build(&crate::player::Diag {
                    abr_mode: crate::player::ABR_MODE_ORIGINAL,
                    abr_unsafe_deficit_ms: ms,
                    ..Default::default()
                }),
                "Action",
            )
        };
        assert_eq!(at(12_000), "collecting fallback evidence · 12 s");
        assert_eq!(at(0), "watching Original");
        assert_ne!(
            v.iter().find(|f| f.key == "Buffer").unwrap().tone,
            crate::ui::widgets::Tone::Fault
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
            abr_declared_kbps: 3_493,
            abr_media_kbps: 3_200,
            video_w: 1_280,
            video_h: 720,
            ..Default::default()
        };
        let v = build(&hls);
        assert_eq!(
            val(&v, "Quality"),
            "request ≤4.0 Mbps / ≤720p · PMS 3.5 Mbps · decoded 1280×720",
        );
        assert_eq!(val(&v, "Sample"), "12.4 Mbps · stream-limited · ±20% · n=7");
        assert_eq!(
            val(&v, "Conservative"),
            "11.0 Mbps budget · 3.2 Mbps measured stream · headroom 7.8 Mbps",
        );
        assert_eq!(val(&v, "Buffer"), "6.2 s · +0.12 s/s · filling");
        assert_eq!(val(&v, "Risk"), "no conservative deficit · risk 0%");
        assert_eq!(val(&v, "Action"), "changed up to 4.0 Mbps");
        assert_eq!(val(&v, "Reason"), "link has room");

        // Device trace, 2026-08-30: the request/controller said 22 Mbps / 4K while PMS declared
        // 896 kbps and every decoded segment was 720x404. The panel must expose all three facts
        // rather than promote a ceiling into a statement about the picture.
        let mismatched = build(&crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_kbps: 22_000,
            abr_declared_kbps: 896,
            abr_media_kbps: 1_051,
            abr_optimal_kbps: 22_000,
            video_w: 720,
            video_h: 404,
            ..Default::default()
        });
        assert_eq!(
            val(&mismatched, "Quality"),
            "request ≤22.0 Mbps / ≤4K · PMS 896 kbps · decoded 720×404",
        );

        // Every actuator on the ladder names its raster, including the ones added after this panel
        // was written — the failure mode is a photograph reading `unknown raster`.
        for rung in crate::abr::LADDER {
            let probe = crate::player::Diag {
                abr_mode: crate::player::ABR_MODE_HLS,
                abr_kbps: i64::from(rung.kbps()),
                abr_optimal_kbps: -1,
                ..Default::default()
            };
            assert!(
                !val(&build(&probe), "Quality").contains("unknown"),
                "{rung:?} has no raster name"
            );
        }

        let probing = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_risk: -1,
            abr_action: crate::player::ABR_ACTION_PROBE_ORIGINAL,
            ..Default::default()
        };
        assert_eq!(val(&build(&probing), "Action"), "checking Original link");
        let recovering = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_risk: -1,
            abr_action: crate::player::ABR_ACTION_RECOVER_ORIGINAL,
            ..Default::default()
        };
        assert_eq!(
            val(&build(&recovering), "Action"),
            "switching back to Original"
        );
        let unavailable = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_risk: -1,
            abr_action: crate::player::ABR_ACTION_ORIGINAL_PROBE_FAILED,
            abr_failure_kind: crate::player::ABR_FAILURE_ORIGINAL_HTTP,
            abr_failure_status: 503,
            ..Default::default()
        };
        assert_eq!(
            val(&build(&unavailable), "Action"),
            "hold · Original check failed · HTTP 503",
        );
        assert_eq!(
            val(&build(&unavailable), "Reason"),
            "PMS refused Original source · HTTP 503",
        );

        let refreshing = crate::player::Diag {
            abr_mode: crate::player::ABR_MODE_HLS,
            abr_action: crate::player::ABR_ACTION_PRIME_REFRESH,
            abr_target_kbps: 22_000,
            abr_why: crate::player::ABR_WHY_RESPONSE_LIMITED,
            ..Default::default()
        };
        assert_eq!(
            val(&build(&refreshing), "Action"),
            "refreshing request 22.0 Mbps"
        );
        assert_eq!(
            val(&build(&refreshing), "Reason"),
            "PMS output below request"
        );

        // A fixed rung keeps the schema. Model-only rows state that they are inactive, while the
        // physical buffer instrument remains present and waits for media timestamps.
        let fixed = build_with(
            &crate::player::Diag::default(),
            crate::route::Quality::Original,
        );
        assert_eq!(fixed.len(), RIGHT_ROWS);
        for key in ["Sample", "Conservative", "Risk"] {
            assert!(
                val(&fixed, key).contains("inactive"),
                "{key} must state fixed mode"
            );
        }
        assert_eq!(val(&fixed, "Buffer"), "waiting for media timestamps");

        // The visual simulator starts its raw fixture with Auto selected but no adaptive session.
        // That state used to read `Auto · starting` beside `fixed by user`, which cannot both be
        // true. It is an idle Auto controller, not a manual choice and not an active estimate.
        let idle_auto = build_with(&crate::player::Diag::default(), crate::route::Quality::Auto);
        assert_eq!(val(&idle_auto, "Mode"), "Auto · controller idle");
        assert_eq!(val(&idle_auto, "Sample"), "not sampled · controller idle");
        assert_eq!(
            val(&idle_auto, "Conservative"),
            "not computed · controller idle"
        );
        assert_eq!(
            val(&idle_auto, "Buffer"),
            "waiting for media timestamps · controller idle"
        );
        assert_eq!(val(&idle_auto, "Risk"), "not computed · controller idle");
        assert_eq!(val(&idle_auto, "Action"), "none · controller idle");
        assert_eq!(val(&idle_auto, "Reason"), "no adaptive session");

        let idle_after_original_failure = build_with(
            &crate::player::Diag {
                abr_failure_kind: crate::player::ABR_FAILURE_ORIGINAL_HTTP,
                abr_failure_status: 500,
                ..Default::default()
            },
            crate::route::Quality::Auto,
        );
        assert_eq!(
            val(&idle_after_original_failure, "Reason"),
            "PMS failed Original source · HTTP 500",
            "a controller restart must not hide the typed source failure behind a generic idle state",
        );

        let hls_backoff = build_with(
            &crate::player::Diag {
                abr_mode: crate::player::ABR_MODE_HLS,
                abr_kbps: 18_600,
                abr_optimal_kbps: 22_000,
                abr_action: crate::player::ABR_ACTION_STEADY,
                abr_why: crate::player::ABR_WHY_REJECT_BACKOFF,
                ..Default::default()
            },
            crate::route::Quality::Auto,
        );
        assert_eq!(val(&hls_backoff, "Action"), "hold · model asks 22.0 Mbps");
        assert_eq!(
            val(&hls_backoff, "Reason"),
            "HLS candidate failed · retry pending",
            "an HLS candidate backoff must not look like an unreported Original failure",
        );
    }

    /// …and it must leave the MAJORITY of the picture visible, or it is the full-screen card
    /// again and "is anything on screen?" — the question, when playback is broken — stops being
    /// answerable while the read-out is up. 40% is the line rather than a third: the codec rows
    /// and the chart are worth the four points, and a corner panel at 35% still shows most of the
    /// frame. What is NOT negotiable is that it stays a corner panel; if this ever needs raising
    /// again, shrink the type instead.
    #[test]
    fn it_leaves_most_of_the_picture_visible() {
        let a = PANEL_W * (HEAD_H + FieldList::height(LEFT_ROWS) + PAD);
        let pct = 100.0 * a / (SCR_W * SCR_H);
        // One column at 30px rows costs about 27% where two at 36px cost 34%. The ceiling stays
        // where it was rather than being tightened onto today's number: a row added tomorrow
        // should cost a row, not a redesign.
        assert!(pct <= 40.1, "panel covers {pct:.0}% of the screen");
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
            fed_a: 4_000, // large, and unmoved since the previous sample
            fed_v_pts: 60_000_000_000,
            fed_a_pts: 30_000_000_000, // 30 s behind
            ..Default::default()
        };
        let v = rows(&d, (4_400, 4_000, 500), 1_000);
        let feed = v.iter().find(|f| f.key == "Feed").expect("Feed row");
        let val = feed.val.as_deref().unwrap_or_default();
        assert!(
            val.contains("1200 fps · 0/s"),
            "the rate must show the dead lane: {val}"
        );

        // The skew has its own stable row now, so it stays at the same coordinate in every mode.
        let sync = v
            .iter()
            .find(|f| f.key == "A/V sync")
            .expect("A/V sync row");
        let sv = sync.val.as_deref().unwrap_or_default();
        assert_eq!(sv, "+30.0 s");
        assert_eq!(
            sync.tone,
            crate::ui::widgets::Tone::Fault,
            "30 s of skew is a fault"
        );
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
        let d = crate::player::Diag {
            load_completed: true,
            load_at: 1_000,
            ..Default::default()
        };
        let fresh = rows(&d, (0, 0, 0), 1_100);
        let f = fresh.iter().find(|f| f.key == "Frames").unwrap();
        assert_ne!(
            f.tone,
            crate::ui::widgets::Tone::Fault,
            "0.1 s after Load is not a stall"
        );
        assert!(f.val.as_deref().unwrap().starts_with("none in 0 s"));

        let stalled = rows(&d, (0, 0, 0), 1_000 + STALL_MS + 4_000);
        let f = stalled.iter().find(|f| f.key == "Frames").unwrap();
        assert_eq!(
            f.tone,
            crate::ui::widgets::Tone::Fault,
            "12 s after Load with no frame IS"
        );
        assert!(f.val.as_deref().unwrap().starts_with("none in 12 s"));
    }

    /// A paused picture is not a stalled one. The verdict line disarms its stall clock while
    /// paused; this pins that the Frames row agrees, because a panel that contradicts itself sends
    /// its reader after a fault that is just the pause button. Reported from the wild on 0.2.1.
    #[test]
    fn a_paused_stream_does_not_report_its_frames_as_frozen() {
        let _g = crate::testlock::serial();
        let d = crate::player::Diag {
            load_completed: true,
            seen_frame: true,
            frames: 190,
            frame_at: 1_000,
            ..Default::default()
        };
        let long_after = 1_000 + STALL_MS + 8_000;

        crate::player::TX.paused.store(true, Ordering::Relaxed);
        let paused = rows(&d, (0, 0, 0), long_after);
        crate::player::TX.paused.store(false, Ordering::Relaxed);
        let playing = rows(&d, (0, 0, 0), long_after);

        let f = |v: &Vec<Field>| {
            v.iter()
                .find(|f| f.key == "Frames")
                .unwrap()
                .val
                .clone()
                .unwrap()
        };
        assert_eq!(f(&paused), "190", "paused: just the count");
        assert!(
            f(&playing).contains("frozen"),
            "playing and not advancing IS frozen: {}",
            f(&playing)
        );
    }

    /// The transport row splits a class the panel could not previously see at all: no connection,
    /// a connection that was refused, and a connection that answered and delivered nothing.
    #[test]
    fn the_http_row_splits_the_open_failures() {
        let row = |d: &crate::player::Diag| {
            rows(d, (0, 0, 0), 1_000)
                .into_iter()
                .find(|f| f.key == "Transfer")
                .unwrap()
        };
        let none = row(&crate::player::Diag::default());
        assert!(none.val.as_deref().unwrap().ends_with("no connection"));

        let refused = row(&crate::player::Diag {
            http_status: 401,
            ..Default::default()
        });
        assert!(refused
            .val
            .as_deref()
            .unwrap()
            .contains("HTTP 401 · 0 B received"));
        assert_eq!(refused.tone, crate::ui::widgets::Tone::Fault);

        // answered fine and delivered bytes — the fault is downstream, and this row says so
        let ok = row(&crate::player::Diag {
            http_status: 200,
            net_rx: 13_000_000,
            cb_count: 4,
            ..Default::default()
        });
        assert!(ok
            .val
            .as_deref()
            .unwrap()
            .contains("HTTP 200 · 12.4 MB received"));
        assert_ne!(ok.tone, crate::ui::widgets::Tone::Fault);
    }

    /// `queue empty` vs `BufferFull` is the row's whole purpose: a dead PRODUCER and a dead SINK
    /// read identically everywhere else on the panel and want opposite fixes. Neither is a fault
    /// tint — both are ordinary moments in a healthy stream; only an outright refusal is.
    #[test]
    fn the_feed_row_splits_a_dead_producer_from_a_dead_sink() {
        let f = |st: u8| crate::player::Diag {
            feed_state: st,
            ..Default::default()
        };
        assert_eq!(f(5).feed_state_str(), "queue empty (no data)");
        assert_eq!(f(2).feed_state_str(), "BufferFull (sink is full)");
        assert!(!f(5).feed_is_fault() && !f(2).feed_is_fault() && !f(4).feed_is_fault());
        assert!(
            f(3).feed_is_fault(),
            "an outright refusal is the only fault"
        );
    }

    /// A latched pipeline error must survive later healthy callbacks — it is the one event that
    /// explains the session — and must carry WHERE it happened, so "refused immediately" and
    /// "died after a long healthy run" are different readings.
    #[test]
    fn a_latched_pipeline_error_outranks_a_healthy_callback_count() {
        let d = crate::player::Diag {
            cb_count: 812,
            cb_err: 18,
            cb_err_at: 4,
            load_completed: true,
            ..Default::default()
        };
        let row = rows(&d, (0, 0, 0), 1_000)
            .into_iter()
            .find(|f| f.key == "Load")
            .unwrap();
        assert!(row
            .val
            .as_deref()
            .unwrap()
            .contains("812 callbacks · ERROR 18 at #4"));
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
        let d = crate::player::Diag {
            fed_v: 10,
            fed_a: 10,
            ..Default::default()
        };
        assert_eq!(fed_rate(&d, (0, 0, 0), 1_000), "—", "no previous sample");
        assert_eq!(
            fed_rate(&d, (0, 0, 2_000), 1_000),
            "—",
            "clock went backwards"
        );
    }

    /// The codec chain: a direct play collapses the middle stage, a real server transform shows
    /// all three, and a payload disagreeing with what is being sent is visible as a mismatch
    /// between the last two — which is the whole reason the row has three stages.
    #[test]
    fn the_codec_chain_shows_the_server_transform_only_when_there_is_one() {
        assert_eq!(chain("h264".into(), "h264".into(), "H264"), "h264 → H264");
        assert_eq!(
            chain("hevc".into(), "h264".into(), "H264"),
            "hevc → h264 → H264"
        );
        // the bug shape: the server re-encoded to h264 and we told the decoder H265
        assert_eq!(
            chain("hevc".into(), "h264".into(), "H265"),
            "hevc → h264 → H265"
        );
        // and nothing known yet must not render as blank gaps around arrows
        assert_eq!(chain(String::new(), String::new(), "—"), "— → —");
    }

    /// The Server row's three arms (issue #22's blind spot made visible). A free server is a FACT
    /// the row states plainly — the fault-tone assertion lives with the row builder test below —
    /// and a server that never named its subscription must not be assigned one either way.
    #[test]
    fn the_server_row_states_the_pass_tristate_without_guessing() {
        use crate::plex::serverinfo::Subscription as S;
        assert_eq!(
            server_line("1.43.3.10861-cd85035e7", S::Yes),
            "1.43.3 · Plex Pass"
        );
        assert_eq!(
            server_line("1.43.3.10861-cd85035e7", S::No),
            "1.43.3 · no Plex Pass"
        );
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
        // Topology and server capability are facts on the stable Connection row, never failures.
        let d = crate::player::Diag::default();
        let row = rows(&d, (0, 0, 0), 1_000)
            .into_iter()
            .find(|f| f.key == "Connection")
            .expect("Connection row");
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
        // "No session" is a PRECONDITION on three crate globals, not a property of a default
        // `Diag`: `player::state()` derives from the pump's `pb_state` and the route's refusal
        // flags, and the hostsim engine tests drive the real pump — which stores `Error` on a
        // refused Load/Play — without restoring it. Establish the state this test asserts about,
        // and hand back whatever was there (see `[[test-suite-global-pollution]]`).
        let prev = crate::player::swap_state_for_test(crate::player::PlaybackState::Idle);
        crate::route::clear_play_verdict_for_test();
        let head = header(&crate::player::Diag::default(), 1_000);
        crate::player::restore_state_for_test(prev);
        // The firmware rides the IDENTITY line now (head[0]); head[1] is the verdict, and the two
        // being one array is what stops the firmware taking the verdict's slot again.
        let line = &head[0];
        if crate::webos::info().major == 0 {
            assert!(line.contains("unknown"), "{line}");
        } else {
            assert!(line.contains("webOS "), "{line}");
        }
        assert!(
            line.contains("surface "),
            "the identity line carries the drawable: {line}"
        );
        // The verdict is a PLAYBACK state, never a firmware string — the regression this pins is
        // the firmware line being drawn where the failure reason belongs.
        assert!(
            !head[1].contains("webOS"),
            "the verdict slot must not carry the firmware: {}",
            head[1]
        );
        assert_eq!(head[1], "Idle", "a default Diag with no session is Idle");
    }
}
