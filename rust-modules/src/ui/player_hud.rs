//! The player transport HUD. Composed from retui widgets (Spinner / TransportButton / TabPill /
//! ProgressBar-style scrubber) drawn through a `Painter`, reading the live playback state via
//! crate::player (TX + playpos_ns/duration_ns) and the route HUD strings via
//! route::title_cptr/ctxline_cptr. The video-overlay subtitle draws below stay on the raw text/tex
//! primitives (they composite directly over the video plane, outside the transport HUD).
#![allow(dead_code)]
use crate::gfx::{delete_tex, upload_rgba};
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::theme;
use crate::ui::widgets::{Spinner, StatusKind, StatusOverlay, TabPill, TransportButton};
use crate::ui::{Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::sync::atomic::Ordering::Relaxed;

/// The HUD widgets draw purely from their own fields and ignore their Env.
fn hud_env() -> Env {
    Env::inert()
}

// The now-playing title under the playbar is a HUD *display* title — deliberately larger than
// `theme::size::TITLE`, so it sits OUTSIDE the shared type scale as a documented carve-out (like the
// subtitle caption in `draw_subtitles`). Both are media chrome with their own legibility contract and
// both are already well above the couch floor; they're named here rather than left as bare literals.
const HUD_TITLE_SZ: i32 = 54;

/// the shared playback clock ([`crate::ui::fmt::clock`]) with the HUD's leading '-' for remaining
fn fmt_time(ns: i64, neg: bool) -> String {
    let c = crate::ui::fmt::clock(ns / 1_000_000);
    if neg {
        format!("-{c}")
    } else {
        c
    }
}

/// naive word-wrap to `max` chars/line (on word boundaries)
fn wrap(s: &str, max: usize) -> Vec<String> {
    if s.chars().count() <= max {
        return vec![s.to_string()];
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// client-rendered subtitle line(s), bottom-center, synced to the video clock. Drawn
/// every frame independent of the transport HUD; hidden when subtitles are off or no
/// cue is active at the current position.
pub(crate) fn draw_subtitles(hud_up: bool) {
    let text = match crate::player::active_subtitle(crate::player::playpos_ns()) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return,
    };
    let mut lines: Vec<String> = Vec::new();
    for seg in text.split('\n') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        for l in wrap(seg, 42) {
            if lines.len() < 3 {
                lines.push(l);
            }
        }
    }
    if lines.is_empty() {
        return;
    }
    let sz = 36; // subtitle caption: media chrome, a documented carve-out from theme::size (see HUD_TITLE_SZ)
    let lh = 48.0f32;
    let n = lines.len() as f32;
    let cx = SCR_W * 0.5;
    // sit near the bottom normally; lift above the scrubber/tabs while the HUD is up
    let baseline = if hud_up { SUB_CEIL_Y } else { SUB_BASE_Y };
    let block_top = baseline - n * lh;
    let white = [1.0f32, 1.0, 1.0, 1.0]; // subtitles stay pure white for legibility (carve-out)
    let outline = theme::scrim_black(0.85);
    let p = Painter::root();
    for (i, ln) in lines.iter().enumerate() {
        let top = block_top + i as f32 * lh;
        if let Ok(cs) = CString::new(ln.as_str()) {
            // dark outline (4 offsets) then bright white bold text — legible over any scene
            for (dx, dy) in [(-2.0f32, 0.0f32), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0)] {
                p.text(cs.as_ptr(), cx + dx, top + 4.0 + dy, sz, outline, 1, 1);
            }
            p.text(cs.as_ptr(), cx, top + 4.0, sz, white, 1, 1);
        }
    }
}

/// Subtitle baseline: where the caption block sits with the transport DOWN, and the ceiling it
/// lifts to with the transport UP (clear of the scrubber, buttons and tabs). Both paths share
/// them: text lifts by moving its baseline, images lift by however much they overhang the
/// ceiling — otherwise a bottom-positioned PGS cue sits behind the scrim while the user seeks.
const SUB_BASE_Y: f32 = SCR_H - 100.0;
const SUB_CEIL_Y: f32 = SCR_H - 300.0;

/// Map a decoded image-subtitle rect from the stream's `cw`×`ch` authoring canvas onto the video
/// rect — which is always the full panel here (the video track is authored 1920×1080; see the
/// root CLAUDE.md). A subtitle canvas is the picture's own storage grid, so this is exactly the
/// stretch the TV applies to the video itself: identity for 1080p PGS, 2.67×/2.25 for a 720×480
/// NTSC VobSub, 0.5× for a 4K PGS track. `cw`/`ch` of 0 means the decoder never declared a canvas
/// — then the rect is used 1:1, which is what this path did unconditionally before.
///
/// Deliberately NOT snapped to whole pixels: this is scaled content, and the crispness contract
/// (root CLAUDE.md) snaps 1:1-texel content only.
fn sub_screen_rect(r: (i32, i32, i32, i32), cw: i32, ch: i32) -> Rect {
    let sx = if cw > 0 { SCR_W / cw as f32 } else { 1.0 };
    let sy = if ch > 0 { SCR_H / ch as f32 } else { 1.0 };
    Rect::new(r.0 as f32 * sx, r.1 as f32 * sy, r.2 as f32 * sx, r.3 as f32 * sy)
}

/// How far UP to shift the rects of a display set that overhang the transport, so they clear it.
/// Nothing moves while the HUD is down, and the shift never exceeds the overhanging group's own
/// top, so a set too tall to lift is clamped rather than pushed off the top of the screen.
///
/// The lift is measured over the OVERHANGING rects only, and (per [`overhangs`]) applies only to
/// them. Measuring the whole set breaks the case multi-rect adds: a set carrying a sign at
/// y≈100 and dialogue at y≈950 would clamp the lift to 100 — sliding the sign for no reason
/// while leaving the dialogue still behind the scrim. Measuring the group and moving it as ONE
/// unit is also why this is not a per-rect lift: two-line dialogue is two rects with different
/// overhangs, and lifting each by its own would collapse them onto each other.
fn hud_lift<I: Iterator<Item = Rect>>(rects: I, hud_up: bool) -> f32 {
    if !hud_up {
        return 0.0;
    }
    let (mut top, mut bottom) = (f32::MAX, f32::MIN);
    for r in rects.filter(|r| overhangs(*r)) {
        top = top.min(r.y);
        bottom = bottom.max(r.y + r.h);
    }
    if top > bottom {
        return 0.0; // nothing overhangs
    }
    (bottom - SUB_CEIL_Y).max(0.0).min(top.max(0.0))
}

/// Does this rect reach below the transport ceiling? Only such rects take the lift.
fn overhangs(r: Rect) -> bool {
    r.y + r.h > SUB_CEIL_Y
}

/// Client-rendered IMAGE subtitles (PGS/VobSub): composite the active decoded display set over
/// the video, scaled from the subtitle stream's own authoring canvas into the video rect
/// (`sub_screen_rect`) and lifted clear of the transport when `hud_up` (`hud_lift`). EVERY rect
/// of the set is drawn — two-line dialogue and sign-plus-dialogue are authored as separate rects.
/// Caches one GL texture per rect and re-uploads only when the active cue changes (every few
/// seconds); the cached rects are the unlifted screen rects, so the lift can follow the HUD
/// frame by frame without re-uploading. Main-thread only (GL). This is the image counterpart to
/// draw_subtitles — a selected track is either text or image, so at most one of the two draws a
/// cue at a time.
pub(crate) fn draw_subtitle_bitmap(hud_up: bool) {
    use std::ptr::addr_of_mut;
    static mut SET: Vec<(c_uint, Rect)> = Vec::new();
    static mut KEY: i64 = i64::MIN;
    // The cache key is (track, start_ns), not start_ns alone: two image tracks of the same file
    // routinely start a display set on the SAME pts, so keying on the timestamp alone leaves the
    // outgoing track's bitmap on screen after a switch between two image tracks.
    static mut SEL: i32 = i32::MIN;
    unsafe {
        let set = &mut *addr_of_mut!(SET);
        let sel = crate::player::desired_sub_idx();
        if sel < 0 {
            for (t, _) in set.drain(..) {
                delete_tex(t);
            }
            KEY = i64::MIN;
            SEL = i32::MIN; // reset BOTH halves of the key — neither should prop the other up
            return;
        }
        match crate::player::active_bitmap_key(crate::player::playpos_ns()) {
            None => KEY = i64::MIN, // gap between cues — draw nothing this frame
            Some(k) => {
                if k != KEY || sel != SEL {
                    let (cw, ch, rects) = match crate::player::bitmap_by_key(k) {
                        Some(v) => v,
                        None => return, // cue evicted between key lookup and fetch
                    };
                    // retire the surplus first, then re-spec the ids we keep: upload_rgba reuses
                    // a non-zero id, so a steady 1-rect stream never allocates a texture twice.
                    for (t, _) in set.drain(rects.len().min(set.len())..) {
                        delete_tex(t);
                    }
                    for (i, r) in rects.iter().enumerate() {
                        let dst = sub_screen_rect((r.x, r.y, r.w, r.h), cw, ch);
                        let prev = set.get(i).map_or(0, |(t, _)| *t);
                        let tex = upload_rgba(prev, r.w, r.h, r.rgba.as_ptr());
                        match set.get_mut(i) {
                            Some(slot) => *slot = (tex, dst),
                            None => set.push((tex, dst)),
                        }
                    }
                    KEY = k;
                    SEL = sel;
                }
                let lift = hud_lift(set.iter().map(|(_, r)| *r), hud_up);
                let white = [1.0f32, 1.0, 1.0, 1.0];
                let p = Painter::root();
                for (tex, r) in set.iter() {
                    let dy = if overhangs(*r) { lift } else { 0.0 };
                    p.tex(*tex, Rect::new(r.x, r.y - dy, r.w, r.h), 0.0, white);
                }
            }
        }
    }
}

// ---- HUD geometry (shared by draw_hud + the pointer hit-tests in app.rs) ----
const SB_X: f32 = 90.0; // scrubber (and title) left margin
pub(crate) const fn sb_w() -> f32 {
    SCR_W - 2.0 * SB_X
}
const SB_Y: f32 = SCR_H - 198.0;
const SB_H: f32 = 8.0;
const BTN_S: f32 = 64.0; // right-side control button size (mockup ≈ 68)
const BTN_GAP: f32 = 22.0;
const BTN_Y: f32 = SCR_H - 288.0;

// The control row, exported for `skip_pill` — while a marker is under the playhead the Skip button
// STANDS IN for the two discs, and it has to land on exactly their row and right edge or the
// transport visibly jumps when a segment begins.
/// control-row height (one disc's diameter)
pub(crate) const CTRL_H: f32 = BTN_S;
/// control-row top
pub(crate) const CTRL_Y: f32 = BTN_Y;
/// control-row right edge — 80px margin, matching the track-menu panel (right:80)
pub(crate) const CTRL_RIGHT: f32 = SCR_W - 80.0;
/// width the Subtitles+Audio pair occupies, the floor for anything replacing them
pub(crate) const CTRL_PAIR_W: f32 = 2.0 * BTN_S + BTN_GAP;

/// The shared control-row slot for anything that STANDS IN for the disc pair: right-aligned to the
/// discs' own edge, and never narrower than the pair it replaces so the row does not visibly shrink
/// when it appears. ONE geometry for both stand-ins and for the pointer hit-test.
///
/// The measured width is memoised per label: `text::text_width` is an uncached `TTF_SizeUTF8`, and
/// the labels are compile-time constants whose width can never change — re-measuring them 2-3× a
/// frame is exactly the thrash `text::elide`'s memo exists to avoid.
pub(crate) fn ctrl_slot(label: &str) -> Rect {
    use std::ptr::addr_of_mut;
    const PAD_X: f32 = 34.0;
    static mut MEMO: Vec<(String, f32)> = Vec::new();
    let memo = unsafe { &mut *addr_of_mut!(MEMO) };
    let w = match memo.iter().find(|(l, _)| l == label) {
        Some((_, w)) => *w,
        None => {
            let measured = CString::new(label)
                .ok()
                .map(|c| crate::text::text_width(c.as_ptr(), theme::size::BODY, 1) + 2.0 * PAD_X)
                .unwrap_or(0.0)
                .max(CTRL_PAIR_W);
            // `text_width` reads 0 until `init_text` has run — don't cache a pre-init measurement
            if measured > CTRL_PAIR_W {
                memo.push((label.to_string(), measured));
            }
            measured
        }
    };
    Rect::new(CTRL_RIGHT - w, CTRL_Y, w, CTRL_H)
}

/// What currently occupies the transport's right-hand control row.
///
/// This is the one concept the skip / Up Next feature introduces, and it exists as a TYPE because
/// the alternative — re-deriving the three-way choice at each of the draw, the OK handler, the
/// pointer handler, the LEFT/RIGHT pin and `icon_hit` — encodes its precedence in the order of five
/// separate if-chains, with nothing to keep them agreeing and nothing to test.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlSlot {
    /// the ordinary Subtitles + Audio pair
    Discs,
    /// a marker segment is under the playhead
    Skip(crate::ui::skip_pill::Prompt),
    /// …and the show has another episode queued, which outranks skipping the credits. Carries the
    /// segment for the same reason `Skip` does — so the row has a stable IDENTITY.
    UpNext(crate::metadata::Marker),
}

impl ControlSlot {
    /// How many focusable items the row holds — what the LEFT/RIGHT clamp needs, as DATA instead of
    /// a hand-written `btn = 0` pin beside a `clamp(0, 1)`.
    pub(crate) fn items(self) -> c_int {
        match self {
            ControlSlot::Discs => 2,
            _ => 1,
        }
    }
    /// whether the Subtitles/Audio discs are the current occupant
    pub(crate) fn is_discs(self) -> bool {
        matches!(self, ControlSlot::Discs)
    }
    /// Which SEGMENT this row is offering, if any — the row's identity for "have I already offered
    /// this?". Deliberately not the `ControlSlot` itself: `active_marker` is gated on `is_playing`,
    /// so a momentary drop out of Playing mid-segment reads as "no segment" and flips the slot to
    /// `Discs` and back. Keyed on the segment, that round trip is not a new offer; keyed on the
    /// slot, it was — and every flicker re-raised the HUD over an intro the user was just watching.
    pub(crate) fn offer(self) -> Option<(crate::metadata::MarkerKind, i64)> {
        match self {
            ControlSlot::Discs => None,
            ControlSlot::Skip(pr) => Some((pr.marker.kind, pr.marker.start_ms)),
            ControlSlot::UpNext(m) => Some((m.kind, m.start_ms)),
        }
    }
    /// Pointer hit-test for whatever occupies the row — ONE entry point, so the click path can
    /// never consult geometry belonging to a control that is not on screen.
    pub(crate) fn hit(self, cx: f32, cy: f32) -> bool {
        match self {
            ControlSlot::UpNext(_) => crate::ui::up_next::hit(cx, cy),
            ControlSlot::Skip(pr) => crate::ui::skip_pill::rect(pr).contains(cx, cy),
            ControlSlot::Discs => false,
        }
    }
}

/// PURE precedence: given the segment under the playhead and whether the queue has a successor,
/// which control owns the row. Host-testable, and the ONLY place the ordering is written down.
///
/// Up Next outranks Skip Credits deliberately: with somewhere to go, "next episode" is the better
/// offer, and Skip Credits stays for the last episode of a show, where there is nowhere to go.
pub(crate) fn slot_for(marker: Option<crate::metadata::Marker>, has_next: bool) -> ControlSlot {
    match marker {
        Some(m) => {
            let pr = crate::ui::skip_pill::prompt_for(m);
            if has_next && m.kind == crate::metadata::MarkerKind::Credits {
                ControlSlot::UpNext(m)
            } else {
                ControlSlot::Skip(pr)
            }
        }
        None => ControlSlot::Discs,
    }
}

/// Sample the live globals ONCE and resolve the row. Call this once per frame and pass the result
/// around: `playpos_ns` is written by LG's media thread and `player::pump` runs between the input
/// handlers and the draw, so re-deriving per call site let a keypress dispatch to a control that
/// the same frame then declined to draw.
pub(crate) fn slot() -> ControlSlot {
    slot_for(crate::metadata::active_marker(), crate::route::up_next().is_some())
}

/// PURE edge: has a stand-in just vanished OUT FROM UNDER the focus ring, so the ring has to go
/// back to the scrubber? `focused` is whether the control row currently holds focus. `was_standin`
/// is the occupant on the last OVERLAY-FREE frame, not simply the last frame — the caller only
/// samples it while the transport is bare, so an edge that happens behind an open track menu / Info
/// card is held and fires on the frame the overlay closes, rather than being lost.
///
/// It exists because the row swapping from a Skip pill back to the discs leaves focus on the row
/// with `btn` still 0, so the next OK opens the SUBTITLES menu instead of toggling pause.
///
/// **It is an EDGE, not a steady state**, and that is the whole reason it is a named function with
/// a test. Written inline as `is_discs() && focused` it is also true on every frame of a user who
/// deliberately walked UP to the Subtitles/Audio discs — the ring is then yanked back to the
/// scrubber the same frame it arrives, so focus can never rest on a disc and OK on one is
/// unreachable by remote. The pointer path never saw it, which is why the on-device suite did not.
pub(crate) fn standin_left_the_ring(was_standin: bool, now: ControlSlot, focused: bool) -> bool {
    was_standin && now.is_discs() && focused
}

/// The bottom scrim's height — the transport's dark ground, transparent at its top edge and
/// `theme::scrim_black(0.86)` at the panel bottom. Named because the read-out's placement is graded
/// against it (see [`readout_frame`] and its test) rather than against a second hand-typed 470.
const SCRIM_H: f32 = 470.0;

/// Which surface owns the "the pipeline is working" signal. There is exactly ONE, ever.
///
/// Two of them used to fire together for the whole of every load and every seek: the centred
/// [`StatusOverlay`] was gated on `is_busy() && frames() == 0`, but `pump` zeroes `frames` as part
/// of APPLYING a seek — so the guard that was meant to say "we have never had a picture" was true
/// for the entire seek, and the transport's own spinner (gated on plain `is_busy()`) was up beside
/// it. Two indicators for one fact read as two facts, which is exactly how it was reported.
///
/// It carries the caption rather than letting each draw re-read `state().caption()`: `Resolving` is
/// derived from `route::play_pending()`, an off-thread flag, so a second read could disagree with
/// the kind this value was chosen for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Busy {
    /// nobody — frames are on the panel, or there is no session
    None,
    /// the centred [`StatusOverlay`] at [`readout_frame`], carrying its treatment + its caption
    Readout(StatusKind, &'static core::ffi::CStr),
    /// the inline spinner beside the elapsed clock
    Transport,
}

/// PURE: which surface owns the busy signal, given the playback state and whether this SESSION has
/// ever presented a frame (`player::seen_frame` — NOT `frames() > 0`, see [`Busy`]).
///
/// **The rule: whichever surface owns the thing being waited for.** No picture yet → the wait is
/// about the whole panel, so the whole panel says so. A picture is up and only the POSITION is in
/// flight → the wait is about the playhead, so the mark goes on the playhead, which the HUD has
/// already frozen at the seek target. `Error` → the centred read-out either way; there is no
/// picture and no position, only a message.
///
/// It takes `seen_frame` rather than the state alone because `Buffering` is genuinely ambiguous —
/// it is both the cold-start tail AND the 1-3 frame tail of every seek (between prime→Play, where
/// `engine` clears `seeking`, and the first presented frame). Keyed on the state alone, one of
/// those two flashes the wrong surface every time.
pub(crate) fn busy_surface(st: crate::player::PlaybackState, seen_frame: bool) -> Busy {
    use crate::player::PlaybackState;
    match st {
        PlaybackState::Error => Busy::Readout(StatusKind::Failed, st.caption()),
        s if s.is_busy() && !seen_frame => Busy::Readout(StatusKind::Working, st.caption()),
        s if s.is_busy() => Busy::Transport,
        _ => Busy::None,
    }
}

/// Sample the live globals ONCE and resolve the owner. Call this once per frame and pass the result
/// to both [`draw_hud`] and [`draw_readout`] — the same discipline [`slot`] keeps, and for the same
/// reason: two independent derivations of one three-way choice is how the two indicators drifted
/// apart in the first place.
pub(crate) fn busy() -> Busy {
    busy_surface(crate::player::state(), crate::player::seen_frame())
}

/// The read-out's frame: **the whole panel**, because the wait is about the whole picture.
///
/// It is deliberately NOT carved down to dodge the transport. The block [`StatusOverlay`] builds is
/// only ~91 px tall, so centred on the panel it spans y≈482…573 — clear of the scrim's top edge at
/// `SCR_H - SCRIM_H` (610) and of the transport's highest ink (the Up Next still at ≈597). The
/// carve-out this replaces (`SCR_H - 340`) centred it at y=370, visibly high, and bought nothing:
/// at 370 the block was already 200 px above a scrim that is fully TRANSPARENT at its top edge
/// anyway. Every "sit above the transport" framing pushes it UP — the un-scrimmed band's own centre
/// is 305 — which is the opposite of the fix the report asked for.
fn readout_frame() -> Rect {
    Rect::FULL
}

/// The load / seek / failure read-out. Drawn EVERY player frame, independent of the transport's
/// auto-hide, because its visibility is the PLAYBACK STATE and not the HUD timer.
///
/// It used to live inside [`draw_hud`], and two things came of that. Historically: `PlaybackState`
/// was published every frame and NOTHING read it, so the initial load drew a live-looking transport
/// at 0:00 / -0:00 and a dead producer drew a fully black screen with no message and no hint that
/// BACK is the way out. Then, once it did read it: in `Error` — which is deliberately not
/// `is_busy()`, hence not covered by `app.rs`'s `|| player::loading()` HUD pin — "Playback failed"
/// disappeared 4.5 s in with the HUD linger, leaving exactly the silent black screen the read-out
/// exists to prevent. A read-out is not transport chrome.
pub(crate) fn draw_readout(busy: Busy, now: u32) {
    let Busy::Readout(kind, caption) = busy else { return };
    StatusOverlay::new(readout_frame(), caption, kind).phase(now).draw(&hud_env(), Painter::root());
}

/// x of control button `idx` (0 = Subtitles on the left, 1 = Audio on the right)
fn btn_x(idx: i32) -> f32 {
    let audio_x = CTRL_RIGHT - BTN_S;
    if idx == 0 {
        audio_x - BTN_S - BTN_GAP
    } else {
        audio_x
    }
}

/// which control button a pointer at (cx,cy) is over: 0 = Subtitles, 1 = Audio, or None.
/// The single source of truth for the button rects, shared with draw_hud.
///
/// `slot` is passed in rather than re-derived: while a stand-in owns the row the discs are not on
/// screen, and a click in that band must not open a track menu the user cannot see.
pub(crate) fn icon_hit(slot: ControlSlot, cx: f32, cy: f32) -> Option<i32> {
    if !slot.is_discs() || cy < BTN_Y || cy > BTN_Y + BTN_S {
        return None;
    }
    (0..2).find(|&idx| {
        let x = btn_x(idx);
        cx >= x && cx <= x + BTN_S
    })
}

/// Pointer hit-test for the scrub-bar grab band (the scrubber's shared geometry, like `icon_hit`
/// for the buttons): `Some(frac 0..1 along the bar)` when (cx,cy) lands in the band. The band is
/// deliberately much taller than the bar itself — a pointer grab zone.
pub(crate) fn scrub_hit(cx: f32, cy: f32) -> Option<f32> {
    let band = cy > SCR_H - 270.0 && cy < SCR_H - 110.0;
    (band && cx >= SB_X && cx <= SB_X + sb_w()).then(|| ((cx - SB_X) / sb_w()).clamp(0.0, 1.0))
}
/// frac along the bar for a drag at `mx` (x only — an engaged drag tracks the pointer even when
/// it wanders off the band vertically).
pub(crate) fn scrub_frac_x(mx: f32) -> f32 {
    ((mx - SB_X) / sb_w()).clamp(0.0, 1.0)
}

// ---- rail marks: the intro/credits segments ---------------------------------------------------
// The marker set already sits in memory during playback and belongs to the PLAYING leaf —
// `metadata::playing_markers()`, which the Skip control reads every frame. It costs no request:
// `?includeMarkers=1` rides the fetch the track store already makes. So this is a draw, not a fetch.
//
// The device constraint shapes the geometry: this Mali is FILL-RATE bound and the HUD composites
// over the transparent UI plane above the hardware video plane, so a mark here is a small opaque
// quad — no glow, no gradient, no per-mark text. At most two segments exist per item (an intro and
// a credits), so the draw cost is bounded by the data rather than by a coalescing rule.
//
// Chapter-boundary ticks were drawn here too and were REMOVED — an owner taste call, not a defect:
// the rail reads cleaner as one continuous bar. The chapter LIST is untouched and is the affordance
// (`ui/chapters_panel.rs`, off the same `metadata::playing_chapters()`), so do not re-add rail ticks
// as a "cheap win" — the backlog entry that proposed them predates the decision.

/// Floor width for a marker band, so a very short segment on a long item is still a visible mark
/// rather than a sub-pixel sliver. The rail's end wins over it: a segment starting in the last few
/// px is drawn short rather than pushed off its own offset.
const MARKER_MIN_W: f32 = 6.0;

/// PURE: rail x for a position in ms, clamped to the rail. Total by construction — an unknown
/// duration (the whole pre-roll, and any item the demuxer never reported one for) maps everything
/// to the rail's start, and the caller below refuses to draw at all in that case.
fn rail_x(ms: i64, dur_ms: i64, sx: f32, sw: f32) -> f32 {
    if dur_ms <= 0 {
        return sx;
    }
    sx + sw * (ms as f64 / dur_ms as f64).clamp(0.0, 1.0) as f32
}

/// PURE: a marker segment's `(left, right)` extent on the rail, or None when it cannot be drawn
/// (unknown duration, or a segment that starts at/after the end of the item).
///
/// A `final` credits marker's stated `end_ms` is the CONTAINER duration and routinely overshoots
/// the decoder's, so the right edge is clamped to the rail rather than allowed to overhang it —
/// the same overshoot [`crate::metadata::marker_at`] absorbs by treating such a segment as
/// open-ended.
fn marker_band(m: crate::metadata::Marker, dur_ms: i64, sx: f32, sw: f32) -> Option<(f32, f32)> {
    if dur_ms <= 0 || sw <= 0.0 || m.start_ms >= dur_ms || m.end_ms <= m.start_ms {
        return None;
    }
    let l = rail_x(m.start_ms.max(0), dur_ms, sx, sw);
    let r = rail_x(m.end_ms, dur_ms, sx, sw).max(l + MARKER_MIN_W).min(sx + sw);
    (r > l).then_some((l, r))
}

/// draw a clock string centred on `cx` as ONE label box, clamped so it stays inside [lo, hi].
/// (The old last-':'-anchor read visibly lopsided once the clock grew to H:MM:SS — "1:05" left of
/// the knob vs "53" right.) The box width is measured on a same-shape template with every digit
/// as '0', so the box — and the returned extents — stay stable while digits tick instead of
/// wobbling with proportional digit widths. Returns the label's (left, right) x extents.
fn draw_clock(p: Painter, text: &str, cx: f32, y: f32, sz: i32, col: [f32; 4], lo: f32, hi: f32) -> (f32, f32) {
    let template: String = text.chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect();
    let w = CString::new(template).ok().map(|t| crate::text::text_width(t.as_ptr(), sz, 1)).unwrap_or(0.0);
    let half = w * 0.5;
    let cx = cx.clamp(lo + half, (hi - half).max(lo + half));
    if let Ok(cs) = CString::new(text) {
        p.text(cs.as_ptr(), cx, y, sz, col, 1, 1);
    }
    (cx - half, cx + half)
}

/// The transport HUD, composed from retui widgets through a root `Painter`.
/// `focus`: 0 = scrubber, 1 = the right control row, 2 = bottom tabs. `btn` (0..1) / `tab` (0..1) are the
/// focused item within their row (only meaningful when `focus` selects that row). `now` drives the
/// loading spinner's rotation. `transport`: draw the scrubber/title/buttons/clocks; pass false for
/// Info mode, where only the scrim + bottom tabs render and the Info card fills the middle.
/// `busy`: the caller's single resolve of who owns the working signal — see [`busy_surface`]; the
/// transport draws its inline spinner only when it is [`Busy::Transport`], and the centred read-out
/// is [`draw_readout`]'s, drawn by the caller AFTER this.
pub(crate) fn draw_hud(slot: ControlSlot, busy: Busy, focus: i32, btn: i32, tab: i32, now: u32, transport: bool) {
    let p = Painter::root();
    let e = hud_env();

    // bottom scrim: transparent -> dark
    let clr = theme::scrim_black(0.0);
    let drk = theme::scrim_black(0.86);
    p.rect(Rect::new(0.0, SCR_H - SCRIM_H, SCR_W, SCRIM_H), 0.0, clr, drk, 0.0);

    let white = theme::TEXT_PRIMARY;
    let dim = theme::TEXT_SECONDARY;
    let track = theme::RAIL_TRACK;

    if transport {
    // title block under the playbar: for an episode, "S1, E1 · Episode Name" (white) sits above the
    // SHOW title; for a movie, the route ctxline over the movie title. (Apple-TV layout.)
    if let Some(n) = crate::metadata::now_playing().filter(|n| n.is_episode) {
        // `fmt::episode_kicker` outright — this line was a byte-identical hand-spelling of it, which
        // is the drift that formatter exists to prevent (the pre-roll ctx line and the Up Next
        // caption already read it, and the whole point is that all three say the same thing).
        if let Ok(cs) = CString::new(crate::ui::fmt::episode_kicker(n.season, n.index, &n.ep_title)) {
            p.text(cs.as_ptr(), SB_X, SCR_H - 312.0, theme::size::CAPTION, white, 0, 1);
        }
        if let Ok(cs) = CString::new(n.title.clone()) {
            p.text(cs.as_ptr(), SB_X, SCR_H - 278.0, HUD_TITLE_SZ, white, 0, 1);
        }
    } else {
        p.text(crate::route::ctxline_cptr(), SB_X, SCR_H - 312.0, theme::size::CAPTION, dim, 0, 0);
        p.text(crate::route::title_cptr(), SB_X, SCR_H - 278.0, HUD_TITLE_SZ, white, 0, 1);
    }

    // The right control row, from the slot the CALLER resolved — so what is drawn and what a
    // keypress activates are the same value, not two derivations of it.
    match slot {
        ControlSlot::UpNext(_) => crate::ui::up_next::draw(p, focus == 1, now),
        ControlSlot::Skip(pr) => crate::ui::skip_pill::draw(p, pr, focus == 1),
        ControlSlot::Discs => {
            TransportButton::new(0, Rect::new(btn_x(0), BTN_Y, BTN_S, BTN_S)).focused(focus == 1 && btn == 0).draw(&e, p);
            TransportButton::new(1, Rect::new(btn_x(1), BTN_Y, BTN_S, BTN_S)).focused(focus == 1 && btn == 1).draw(&e, p);
        }
    }

    // scrubber
    let sx = SB_X;
    let sw = sb_w();
    let sy = SB_Y;
    let sh = SB_H;
    let scrub = crate::player::TX.scrub_ns.load(Relaxed);
    // while a seek is loading, freeze the playhead at the target (no wobble through the reopen);
    // else follow the live scrub preview, else the real playhead.
    let loading = crate::player::loading();
    let dispos = if loading && crate::player::seek_display_ns() >= 0 {
        crate::player::seek_display_ns()
    } else if scrub >= 0 {
        scrub
    } else {
        crate::player::playpos_ns()
    };
    let dur = crate::player::duration_ns();
    let frac = if dur > 0 { (dispos as f64 / dur as f64).clamp(0.0, 1.0) } else { 0.0 };
    p.rect(Rect::new(sx, sy, sw, sh), sh * 0.5, track, track, 0.0);
    let dur_ms = dur / 1_000_000;
    // intro / credits segments, UNDER the fill: a segment already watched should read as watched,
    // exactly like the rest of the rail. At most two per item (an intro and a credits).
    for m in crate::metadata::playing_markers() {
        if let Some((l, r)) = marker_band(*m, dur_ms, sx, sw) {
            let rad = (sh * 0.5).min((r - l) * 0.5);
            p.rrect(Rect::new(l, sy, r - l, sh), rad, rad, theme::RAIL_MARKER);
        }
    }
    let fw = (sw as f64 * frac) as f32;
    if fw > sh * 0.5 {
        p.rrect(Rect::new(sx, sy, fw, sh), sh * 0.5, 0.0, white);
    } else if fw > 0.0 {
        p.rrect(Rect::new(sx, sy, fw, sh), fw * 0.5, 0.0, white);
    }
    // playhead: a focus-glowing knob when the scrubber is focused, a plain knob while scrubbing,
    // else a thin tick.
    let hx = sx + fw;
    let cy = sy + sh * 0.5;
    if focus == 0 {
        let glow = [1.0f32, 1.0, 1.0, 0.22];
        p.rect(Rect::new(hx - 17.0, cy - 17.0, 34.0, 34.0), 17.0, glow, glow, 0.0);
        p.rect(Rect::new(hx - 11.0, cy - 11.0, 22.0, 22.0), 11.0, white, white, 0.0);
    } else if scrub >= 0 {
        p.rect(Rect::new(hx - 9.0, cy - 9.0, 18.0, 18.0), 9.0, white, white, 0.0);
    } else {
        p.rect(Rect::new(hx - 1.5, sy - 4.0, 3.0, sh + 8.0), 0.0, white, white, 0.0);
    }

    // elapsed under the playhead (':' centered on the knob, clamped to the bar); remaining at the
    // right — hidden once the moving elapsed label would overlap it (near the end of the movie).
    let ty = sy + 30.0;
    let (el_l, el_r) = draw_clock(p, &fmt_time(dispos, false), hx, ty, theme::size::CAPTION, white, sx, sx + sw);
    let rem = fmt_time(dur - dispos, true);
    // MEASURE the label, never estimate it. `chars * CAPTION * 0.52` was an Arial-calibrated
    // constant guarding a real behaviour — the remaining clock hides before the moving elapsed
    // clock can reach it — and it only ever worked because it over-estimated: under Arial it
    // returned ~99.8px against a true ~85px. Under the shipped Inter that margin had already
    // halved, and freezing tabular figures (which the clock's own template idiom requires, see
    // `draw_clock`) widens numerals further, leaving about a pixel of slack. At that point the
    // guard stops guarding and the two clocks can overlap near the end of a long item.
    // Same '0'-template `draw_clock` uses, so the two agree by construction: with tabular figures
    // the template's width IS the real string's width at every tick.
    let rem_tmpl: String = rem.chars().map(|c| if c.is_ascii_digit() { '0' } else { c }).collect();
    let rem_w = CString::new(rem_tmpl)
        .ok()
        .map(|t| crate::text::text_width(t.as_ptr(), theme::size::CAPTION, 1))
        .unwrap_or(0.0);
    let rem_l = sx + sw - rem_w;
    let rem_shown = el_r + 20.0 < rem_l;
    if rem_shown {
        if let Ok(cs) = CString::new(rem.as_str()) {
            p.text(cs.as_ptr(), sx + sw, ty, theme::size::CAPTION, dim, 2, 0);
        }
    }
    // transport state indicator just past the elapsed clock — a Pause glyph while paused, a seek
    // spinner while THIS surface owns the busy signal, and NOTHING while playing (a state read-out,
    // not an action toggle). Gated on `busy`, not on `loading()`: with `loading()` the transport lit
    // the same spinner the centred read-out was already showing, for the whole of every load AND
    // every seek. Centered on the clock's line box; drops to the clock's LEFT when the right side is
    // against the remaining label / screen edge.
    let paused = crate::player::TX.paused.load(Relaxed);
    let seeking = busy == Busy::Transport;
    if seeking || paused {
        // pause bars under-fill their viewBox (14/24 tall) — a 30px box renders ~17px of ink,
        // matching the CAPTION clock's cap height so the glyph reads as the label's size.
        let isz = 30.0f32;
        let need = isz + 6.0;
        let right_ok = el_r + 14.0 + need < if rem_shown { rem_l - 8.0 } else { sx + sw };
        let gx = if right_ok { el_r + 14.0 } else { el_l - 14.0 - need };
        let icy = ty + crate::text::text_height(theme::size::CAPTION, 1) * 0.5; // vertical center of the clock line
        if seeking {
            Spinner::new(gx + isz * 0.5, icy, Spinner::R_INLINE).phase(now).tint(white).draw(&e, p);
        } else {
            crate::ui::icons::draw(p, crate::ui::icons::Icon::Pause, Rect::new(gx, icy - isz * 0.5, isz, isz), white);
        }
    }
    } // end `if transport`

    // bottom tabs as pills — Chapters only appears when the item actually has chapters
    let tabs: &[&str] = if crate::ui::chapters_panel::has_chapters() {
        &["Info", "Chapters"]
    } else {
        &["Info"]
    };
    // tabs match the transport control buttons' height (BTN_S), centred vertically between the
    // play bar (scrubber, at SB_Y) and the bottom edge of the screen
    let ph = BTN_S; // = 64, same as the Subtitles/Audio buttons
    let py = (SB_Y + SCR_H) * 0.5 - ph * 0.5;
    let mut px = SB_X;
    for (i, label) in tabs.iter().enumerate() {
        let on = focus == 2 && tab == i as i32;
        let pw = TabPill::width(label.chars().count(), theme::size::BODY);
        if let Ok(cs) = CString::new(*label) {
            TabPill::new(cs.as_ptr(), theme::size::BODY, Rect::new(px, py, pw, ph)).focused(on).draw(&e, p);
        }
        px += pw + 16.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{Marker, MarkerKind};
    use crate::ui::skip_pill::SkipAction;

    fn marker(kind: MarkerKind, final_seg: bool) -> Marker {
        Marker { kind, start_ms: 1_000, end_ms: 2_000, final_seg }
    }

    fn seg(start_ms: i64, end_ms: i64, final_seg: bool) -> Marker {
        Marker { kind: MarkerKind::Credits, start_ms, end_ms, final_seg }
    }

    // A rail 1000px wide over a 1000ms item: 1px per ms, so an offset reads straight off the x.
    const SX: f32 = 100.0;
    const SW: f32 = 1000.0;
    const DUR: i64 = 1_000;

    /// The control row's precedence, which used to live in the ORDER of five separate if-chains
    /// across `player_hud` and `app.rs` — the draw, the OK handler, the pointer handler, the
    /// LEFT/RIGHT clamp and `icon_hit` — with nothing keeping them in step and nothing to test.
    #[test]
    fn the_control_row_picks_the_most_specific_occupant() {
        // no segment under the playhead → the ordinary Subtitles + Audio pair
        assert!(slot_for(None, false).is_discs());
        assert!(slot_for(None, true).is_discs(), "a queued successor alone changes nothing");
        assert_eq!(slot_for(None, true).items(), 2, "the disc pair is the only two-item row");

        // an intro is always Skip, successor or not — "what's next" is an end-of-episode idea
        for has_next in [false, true] {
            let slot = slot_for(Some(marker(MarkerKind::Intro, false)), has_next);
            assert!(matches!(slot, ControlSlot::Skip(p) if p.kind == MarkerKind::Intro));
            assert_eq!(slot.items(), 1, "a stand-in is the row's only item");
        }

        // credits WITH somewhere to go → Up Next outranks Skip Credits…
        assert!(matches!(slot_for(Some(marker(MarkerKind::Credits, true)), true), ControlSlot::UpNext(_)));
        // …and WITHOUT (a show's last episode) it stays Skip Credits, which is the whole reason
        // both still exist
        assert!(matches!(
            slot_for(Some(marker(MarkerKind::Credits, true)), false),
            ControlSlot::Skip(p) if p.kind == MarkerKind::Credits
        ));
    }

    /// A `final` credits segment runs to the end of the item, so skipping it FINISHES rather than
    /// seeks — seeking to its stated end would race the decoder against its own last frames.
    #[test]
    fn only_a_final_credits_segment_finishes_the_item() {
        let fin = match slot_for(Some(marker(MarkerKind::Credits, true)), false) {
            ControlSlot::Skip(p) => p.action,
            _ => unreachable!(),
        };
        assert_eq!(fin, SkipAction::Finish);

        // a mid-item credits segment (a post-credits scene follows) seeks past it and plays on
        let mid = match slot_for(Some(marker(MarkerKind::Credits, false)), false) {
            ControlSlot::Skip(p) => p.action,
            _ => unreachable!(),
        };
        assert_eq!(mid, SkipAction::Seek(2_000 * 1_000_000));
        // …as does an intro, `final` flag or not (PMS only sets it on credits)
        let intro = match slot_for(Some(marker(MarkerKind::Intro, true)), false) {
            ControlSlot::Skip(p) => p.action,
            _ => unreachable!(),
        };
        assert_eq!(intro, SkipAction::Seek(2_000 * 1_000_000));
    }

    /// The intro/credits band covers its own segment, and a `final` credits marker — whose stated
    /// end is the CONTAINER duration, which routinely overshoots the decoder's — stops at the rail's
    /// end instead of overhanging it.
    #[test]
    fn a_marker_band_covers_its_segment_and_stops_at_the_rail() {
        assert_eq!(marker_band(seg(100, 200, false), DUR, SX, SW), Some((200.0, 300.0)));

        // a `final` credits segment running to (or past) the container duration
        assert_eq!(marker_band(seg(900, 1_000, true), DUR, SX, SW), Some((1_000.0, 1_100.0)));
        assert_eq!(marker_band(seg(900, 1_200, true), DUR, SX, SW), Some((1_000.0, 1_100.0)));

        // a segment shorter than the floor is widened to it rather than drawn sub-pixel
        let (l, r) = marker_band(seg(500, 501, false), DUR, SX, SW).expect("a 1ms segment still draws");
        assert_eq!((l, r), (600.0, 600.0 + MARKER_MIN_W));

        // nothing to draw: no duration, a segment that starts at/after the end, an inverted one
        assert_eq!(marker_band(seg(100, 200, false), 0, SX, SW), None);
        assert_eq!(marker_band(seg(1_000, 1_100, false), DUR, SX, SW), None);
        assert_eq!(marker_band(seg(300, 300, false), DUR, SX, SW), None);
    }
    /// The ring only goes back to the scrubber on the EDGE where a stand-in vanished under it.
    /// As a steady state (`is_discs() && focused`, which is how it shipped) it fired on every
    /// frame the user had walked UP to the Subtitles/Audio discs, so focus could not rest there
    /// and OK on a disc never reached the track menu — invisible to the pointer path, and so to
    /// the on-device suite.
    #[test]
    fn only_a_vanishing_standin_takes_the_focus_ring_back() {
        let discs = slot_for(None, false);
        let skip = slot_for(Some(marker(MarkerKind::Intro, false)), false);

        // the edge it exists for: the pill was there last frame, the discs are back, ring on the row
        assert!(standin_left_the_ring(true, discs, true));

        // the steady state it must NOT fire on: the discs were already there, so nothing vanished
        assert!(!standin_left_the_ring(false, discs, true), "walking UP to the discs is not a lost stand-in");

        // nothing to take back if the ring is elsewhere, or if a stand-in still owns the row
        assert!(!standin_left_the_ring(true, discs, false));
        assert!(!standin_left_the_ring(true, skip, true));
    }
    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }
    fn assert_rect(r: Rect, x: f32, y: f32, w: f32, h: f32) {
        assert!(
            close(r.x, x) && close(r.y, y) && close(r.w, w) && close(r.h, h),
            "got ({}, {}, {}, {}), want ({x}, {y}, {w}, {h})",
            r.x,
            r.y,
            r.w,
            r.h
        );
    }

    /// A 1080p-authored PGS rect must land EXACTLY where it always did — the scale is identity,
    /// not merely close to it, or every Blu-ray subtitle in the library moves a pixel.
    #[test]
    fn image_sub_1080p_canvas_is_identity() {
        assert_rect(sub_screen_rect((300, 900, 1320, 96), 1920, 1080), 300.0, 900.0, 1320.0, 96.0);
    }

    /// The bug this unit exists for: a DVD VobSub rip is authored on a 720×480 canvas, so drawn
    /// 1:1 it is a postage stamp covering the top-left third of the panel. Scaled from its own
    /// canvas it spans the picture and sits at the bottom, exactly like the PGS case above.
    #[test]
    fn image_sub_dvd_canvas_fills_the_panel() {
        // a full-width, near-bottom VobSub line
        assert_rect(sub_screen_rect((0, 400, 720, 60), 720, 480), 0.0, 900.0, 1920.0, 135.0);
        // and an inset one keeps its position within the picture
        let r = sub_screen_rect((180, 240, 360, 48), 720, 480);
        assert_rect(r, 480.0, 540.0, 960.0, 108.0);
        assert!(close(r.x + r.w * 0.5, SCR_W * 0.5), "a canvas-centred rect stays centred");
    }

    /// PAL (720×576) and 4K PGS (3840×2160) are the other two canvases in the wild; the 4K one
    /// scales DOWN, which the old 1:1 path drew at double size and half off-screen.
    #[test]
    fn image_sub_pal_and_4k_canvases() {
        assert_rect(sub_screen_rect((0, 480, 720, 72), 720, 576), 0.0, 900.0, 1920.0, 135.0);
        assert_rect(sub_screen_rect((600, 1800, 2640, 192), 3840, 2160), 300.0, 900.0, 1320.0, 96.0);
    }

    /// A decoder that never declares a canvas (0) must fall back to 1:1 — the behaviour of the
    /// whole path before this change — rather than divide by zero or blank the cue.
    #[test]
    fn image_sub_unknown_canvas_stays_1to1() {
        assert_rect(sub_screen_rect((300, 900, 1320, 96), 0, 0), 300.0, 900.0, 1320.0, 96.0);
        assert_rect(sub_screen_rect((300, 900, 1320, 96), -1, 1080), 300.0, 900.0, 1320.0, 96.0);
    }

    /// The HUD lift: image cues rise just enough to clear the transport, and only then.
    #[test]
    fn image_sub_lifts_only_over_the_transport() {
        let dialogue = Rect::new(300.0, 900.0, 1320.0, 135.0); // bottom 1035, overhangs
        // transport down: nothing moves, ever
        assert_eq!(hud_lift([dialogue].into_iter(), false), 0.0);
        // transport up: a bottom-anchored cue rises exactly its overhang, landing ON the ceiling
        let lift = hud_lift([dialogue].into_iter(), true);
        assert!(close(1035.0 - lift, SUB_CEIL_Y), "lifted bottom should sit at the ceiling");
        // a sign at the top of frame already clears it and must stay put
        let sign = Rect::new(100.0, 100.0, 200.0, 100.0);
        assert_eq!(hud_lift([sign].into_iter(), true), 0.0);
        assert!(!overhangs(sign) && overhangs(dialogue));
        // a set too tall to lift is clamped instead of being pushed off the top of the screen
        assert_eq!(hud_lift([Rect::new(0.0, 0.0, 1920.0, SCR_H)].into_iter(), true), 0.0);
        assert_eq!(hud_lift([Rect::new(0.0, 50.0, 1920.0, SCR_H - 50.0)].into_iter(), true), 50.0);
    }

    /// The multi-rect case the lift has to get right, and the reason it is measured over the
    /// OVERHANGING rects rather than the whole set: a sign plus dialogue must lift the dialogue
    /// clear WITHOUT dragging the sign (whose top would otherwise clamp the whole shift), and
    /// two-line dialogue must move as one unit or the two lines collapse onto each other.
    #[test]
    fn image_sub_lift_spans_only_the_rects_that_overhang() {
        let sign = Rect::new(100.0, 100.0, 200.0, 100.0);
        let dialogue = Rect::new(300.0, 900.0, 1320.0, 135.0);
        let lift = hud_lift([sign, dialogue].into_iter(), true);
        assert!(close(lift, 1035.0 - SUB_CEIL_Y), "the sign's top must not clamp the lift");
        assert!(close(dialogue.y + dialogue.h - lift, SUB_CEIL_Y), "dialogue clears the transport");
        assert!(!overhangs(sign), "and the sign is not shifted at all");

        // two-line dialogue: different overhangs, ONE shift, so the gap between them survives
        let l1 = Rect::new(300.0, 900.0, 1320.0, 50.0); // bottom 950
        let l2 = Rect::new(300.0, 960.0, 1320.0, 50.0); // bottom 1010
        let two = hud_lift([l1, l2].into_iter(), true);
        assert!(close(two, 1010.0 - SUB_CEIL_Y), "measured over the group, not per rect");
        assert!(overhangs(l1) && overhangs(l2), "both take the same shift");
        assert!(close((l2.y - two) - (l1.y + l1.h - two), 10.0), "their 10px gap is preserved");
    }

    // ---- who owns the "the pipeline is working" signal -----------------------------------------

    use crate::player::PlaybackState as S;

    /// Every state the enum has, so a state added later cannot quietly escape the table below.
    const ALL_STATES: [S; 7] =
        [S::Idle, S::Resolving, S::Connecting, S::Buffering, S::Seeking, S::Playing, S::Error];

    /// The whole point of routing both surfaces through one function: for any state, and either
    /// answer to "has this session shown a picture", there is exactly ONE indicator — never two
    /// (the reported bug) and never none while the pipeline is working.
    ///
    /// Driven off `is_busy()` itself rather than a hand-copied list, so a FUTURE busy state cannot
    /// silently lose its indicator; the membership guard underneath pins what `is_busy()` means, so
    /// the coverage claim can't be satisfied by quietly narrowing it.
    #[test]
    fn exactly_one_surface_owns_the_busy_signal() {
        for st in ALL_STATES {
            for seen in [false, true] {
                let b = busy_surface(st, seen);
                if st.is_busy() || st == S::Error {
                    assert_ne!(b, Busy::None, "{st:?}/seen={seen} lost its indicator");
                } else {
                    assert_eq!(b, Busy::None, "{st:?}/seen={seen} must show nothing");
                }
            }
        }
        for st in ALL_STATES {
            let want = matches!(st, S::Resolving | S::Connecting | S::Buffering | S::Seeking);
            assert_eq!(st.is_busy(), want, "{st:?} changed sides of is_busy()");
        }
    }

    /// The regression, named after the bug. The shipped guard was `is_busy() && frames() == 0`, and
    /// `pump` zeroes `frames` as part of APPLYING a seek — so the centred read-out fired on every
    /// seek, on top of the transport's own spinner. Keyed on the SESSION bit instead, a seek over a
    /// live picture is the transport's alone.
    #[test]
    fn a_seek_over_a_live_picture_belongs_to_the_transport() {
        assert_eq!(busy_surface(S::Seeking, true), Busy::Transport);
        assert!(!matches!(busy_surface(S::Seeking, true), Busy::Readout(..)));
    }

    /// The 1-3 frame window between `engine`'s prime→Play clear of `seeking` and the first
    /// presented frame, which the pump publishes as `Buffering`. Keyed on the state alone this
    /// flashes the centred block at the END of every seek; it is precisely why the rule takes
    /// `seen_frame`, and without the assertion someone will "simplify" it back.
    #[test]
    fn the_post_seek_buffering_tail_does_not_flash_the_centre_readout() {
        assert_eq!(busy_surface(S::Buffering, true), Busy::Transport);
        assert_eq!(busy_surface(S::Resolving, true), Busy::Transport, "auto-advance over a live picture");
        assert_eq!(busy_surface(S::Connecting, true), Busy::Transport);
    }

    /// No picture yet → the wait is about the whole panel. With the captions, which locks the
    /// kind↔caption pairing the enum carries (both are chosen once, by this function).
    #[test]
    fn a_cold_start_and_a_reload_both_own_the_whole_panel() {
        assert_eq!(busy_surface(S::Resolving, false), Busy::Readout(StatusKind::Working, c"Preparing\u{2026}"));
        assert_eq!(busy_surface(S::Connecting, false), Busy::Readout(StatusKind::Working, c"Connecting\u{2026}"));
        assert_eq!(busy_surface(S::Buffering, false), Busy::Readout(StatusKind::Working, c"Buffering\u{2026}"));
        // tapping RIGHT during pre-roll (or `/tmp/plxnative-autoseek`): no picture to mark up
        assert_eq!(busy_surface(S::Seeking, false), Busy::Readout(StatusKind::Working, c"Seeking\u{2026}"));
    }

    /// A dead producer has no picture and no position, only a message — so it is the centred
    /// read-out either way. And it is a FAULT, never `Empty`, which is the "this is an answer"
    /// treatment.
    #[test]
    fn a_dead_producer_reads_out_whether_or_not_a_picture_was_up() {
        for seen in [false, true] {
            assert_eq!(busy_surface(S::Error, seen), Busy::Readout(StatusKind::Failed, c"Playback failed"));
        }
    }

    /// The geometry regression, guarding against a new `OVERLAY_BOTTOM`: the read-out centres on
    /// the PANEL, and doing so still clears the transport — the constraint the carve-out claimed to
    /// be enforcing and was not.
    ///
    /// The spinner half of the block is pure geometry (the caption half needs `text_height`, which
    /// the host suite cannot link) and is the LARGER half, so `above()` is a conservative stand-in
    /// for the whole block. Numerically 540 + 58.16 = 598.16 < 610: deliberately tight, so it fails
    /// if someone bumps `R_PAGE`, thickens the scrim, or re-carves the frame.
    #[test]
    fn the_readout_is_centred_and_still_clears_the_transport() {
        let f = readout_frame();
        assert_eq!(f.cy(), SCR_H * 0.5, "the wait is about the whole picture, so it centres on it");
        assert_eq!(f.cx(), SCR_W * 0.5);
        assert!(
            f.cy() + StatusOverlay::above() < SCR_H - SCRIM_H,
            "the read-out must not reach into the transport scrim"
        );
        assert!(f.cy() - StatusOverlay::above() > 0.0, "nor off the top of the panel");
    }
}
