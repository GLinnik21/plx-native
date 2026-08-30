//! The player transport HUD. Composed from retui widgets (Spinner / TransportButton / TabPill /
//! ProgressBar-style scrubber) drawn through a `Painter`, reading the live playback state via
//! crate::player (TX + playpos_ns/duration_ns) and the route HUD strings via
//! route::title_cptr/ctxline_cptr. The video-overlay subtitle draws below stay on the raw text/tex
//! primitives (they composite directly over the video plane, outside the transport HUD).
#![allow(dead_code)]
use crate::gfx::{delete_tex, upload_rgba};
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::theme;
use crate::ui::widgets::{ControlGround, Spinner, StatusKind, StatusOverlay, TabPill, TransportButton};
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
/// `docs/agent-reference.md`). A subtitle canvas is the picture's own storage grid, so this is exactly the
/// stretch the TV applies to the video itself: identity for 1080p PGS, 2.67×/2.25 for a 720×480
/// NTSC VobSub, 0.5× for a 4K PGS track. `cw`/`ch` of 0 means the decoder never declared a canvas
/// — then the rect is used 1:1, which is what this path did unconditionally before.
///
/// Deliberately NOT snapped to whole pixels: this is scaled content, and the crispness contract
/// (`docs/agent-reference.md`) snaps 1:1-texel content only.
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
// scrubber (and title, and the bottom tab pills) left margin — the app's own, not a second copy of
// it: this was a literal 90 and so stayed put when `MARGIN_X` moved to the overscan-safe 96
const SB_X: f32 = crate::ui::consts::MARGIN_X;
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
/// control-row right edge — the app's side margin, matching the track-menu panel.
///
/// The mock's `right: 80` put the Subtitles/Audio/`…` discs 16px past the 5% overscan frame, and the
/// track and `…` panels were aligned to the same 80 (`track_menu`/`more_menu`). All three moved onto
/// `MARGIN_X` together on 2026-08-23 — they are one right edge by design, so they take one number.
pub(crate) const CTRL_RIGHT: f32 = SCR_W - crate::ui::consts::MARGIN_X;
/// how many control discs the row holds: Subtitles, Audio, More
const BTN_N: i32 = 3;

/// The CONTROL ROW's focus pop — one spring per item ([`crate::ui::widgets::CtlPop`]), so the
/// control being left shrinks while the arriving one grows instead of both snapping.
///
/// **One array for all three slot occupants**, not one per module. The transport discs, the Skip
/// pill and the Up Next pair are the same row at the same right edge, only ever one at a time, and
/// the cursor walking them is `hud_nav.btn` for all three ([`ControlSlot::items`]) — so the pop is a
/// property of that row and not of whichever module happens to draw it this frame. `BTN_N` 3 is the
/// widest occupant. `up_next` and `skip_pill` read theirs through [`row_pop`].
///
/// A static because the HUD keeps no view struct: it is drawn entirely from the caller's arguments,
/// which is right for everything else it does and leaves this the one piece of retained motion state
/// on the route.
static mut ROW_POP: crate::ui::widgets::CtlPop<{ BTN_N as usize }> =
    crate::ui::widgets::CtlPop::new();

/// Step the control row's focus pop — once per frame, from `app.rs`'s update phase.
///
/// **Not from [`draw_hud`]**, though that is where every other number this row draws comes from: a
/// spring advanced inside a draw advances once per DRAW, and this row is not drawn on every frame of
/// the route (a failure read-out owns the frame, the Info card and the Chapters strip take the
/// transport away). The pop would then run at a rate that depended on which overlay was open.
///
/// `focus == 1` is the control column; anything else closes every pop. The index is bounded by the
/// CURRENT occupant's item count, so a stale `btn` left over from a wider slot cannot pop a control
/// the narrower one does not have.
///
/// It also steps the state read-out's resume clock ([`note_transport`]) — the row's other piece of
/// retained motion state, and one that is a raw CLOCK rather than a spring, so it needs this
/// once-per-frame call for exactly the reason spelled out above.
pub(crate) fn update(slot: ControlSlot, focus: i32, btn: i32, dt: f32, now: u32) {
    note_transport(now);
    let f = (focus == 1)
        .then(|| usize::try_from(btn).ok())
        .flatten()
        .filter(|&i| i < slot.items().max(0) as usize);
    unsafe { (*std::ptr::addr_of_mut!(ROW_POP)).step(f, dt) };
}

/// Control `i`'s focus pop this frame — the read half of [`ROW_POP`], for the two stand-ins that
/// draw into this row from their own modules (`up_next`, `skip_pill`).
pub(crate) fn row_pop(i: c_int) -> f32 {
    unsafe { std::ptr::addr_of!(ROW_POP).as_ref().unwrap().scale(i.max(0) as usize) }
}
/// Index of the `…` overflow disc within the row — the LAST one. Exported because `app.rs` routes
/// its OK and its click, and a second literal `2` over there is exactly the drift `ControlSlot`
/// was introduced to stop.
pub(crate) const BTN_MORE: i32 = BTN_N - 1;
/// width the disc row occupies, the floor for anything replacing it
pub(crate) const CTRL_ROW_W: f32 = 3.0 * BTN_S + 2.0 * BTN_GAP;

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
                .max(CTRL_ROW_W);
            // `text_width` reads 0 until `init_text` has run — don't cache a pre-init measurement
            if measured > CTRL_ROW_W {
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
            ControlSlot::Discs => BTN_N,
            // Watch Credits + Next Episode — the one stand-in with a pair
            ControlSlot::UpNext(_) => 2,
            ControlSlot::Skip(_) => 1,
        }
    }
    /// Where the focus ring parks when this row APPEARS. It is 0 everywhere except Up Next, whose
    /// primary is drawn on the RIGHT — and getting that wrong is not cosmetic: the countdown's
    /// cancel rule is a steady state (`up_next::countdown_may_run`), so parking on item 0 there
    /// would disarm the timer on the frame after it armed.
    pub(crate) fn primary_btn(self) -> c_int {
        match self {
            ControlSlot::UpNext(_) => crate::ui::up_next::PRIMARY_BTN,
            _ => 0,
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
    /// never consult geometry belonging to a control that is not on screen. It answers with the
    /// ITEM index rather than a bool because Up Next has two: the click has to park `hud_nav.btn`
    /// before dispatching, or the shared `activate_ctrl_row` would act on wherever the ring
    /// happened to be rather than on what was clicked.
    pub(crate) fn hit(self, cx: f32, cy: f32) -> Option<c_int> {
        match self {
            ControlSlot::UpNext(_) => crate::ui::up_next::hit(cx, cy),
            ControlSlot::Skip(pr) => crate::ui::skip_pill::rect(pr).contains(cx, cy).then_some(0),
            ControlSlot::Discs => None,
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
    let has_next = crate::route::up_next().is_some();
    // Server marker first; the synthesized tail only exists where credits DETECTION does not
    // (a Plex Pass server feature) — see `metadata::synthesized_tail_marker`.
    let m = crate::metadata::active_marker().or_else(|| crate::metadata::synthesized_tail_marker(has_next));
    slot_for(m, has_next)
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
        // not `st.caption()`: the Error caption is shaped by WHY (an audio-only stream names the
        // server; see `player::error_shape`), which a method on the bare state cannot know.
        PlaybackState::Error => Busy::Readout(StatusKind::Failed, crate::player::error_caption()),
        s if s.is_busy() && !seen_frame => Busy::Readout(StatusKind::Working, st.caption()),
        s if s.is_busy() => Busy::Transport,
        _ => Busy::None,
    }
}

/// PURE: does the read-out own the WHOLE frame, so [`draw_hud`] draws nothing at all?
///
/// **Only a failure**, and the asymmetry is the point. A `Failed` read-out is terminal: there is no
/// picture and no position, so every piece of transport under it is a lie — `Player Screen.dc.html`
/// hides both the transport (`hudDisplay:none`) and the bottom tabs (`tabsDisplay:none`) on that
/// variant. A `Working` read-out is the opposite: the pipeline is mid-flight, the position is real,
/// and the transport is what the user reads the moment the first frame lands. Hiding it there would
/// blank the HUD through every cold start and every pre-roll — which is why this asks the KIND and
/// not merely "is a read-out up".
fn readout_owns_frame(busy: Busy) -> bool {
    matches!(busy, Busy::Readout(StatusKind::Failed, _))
}

/// The same question for `app.rs`'s INPUT arms: is the transport (and every panel over it) absent
/// from the frame right now?
///
/// A control that is not drawn must not be activatable — the rule [`ControlSlot::hit`] keeps for
/// the pointer, at the row's own altitude. Without this the failure was hidden but
/// still drivable: `start_playback` stamps a ~4.5 s HUD linger on the way in, so a `/decision`
/// refusal lands with `hud_visible` true and focus parked on the scrubber, and two blind presses
/// (DOWN, OK) opened the Info card over the read-out from a tab row nothing had painted.
///
/// It resamples [`busy`] rather than taking one, because the event loop runs before the frame's
/// single resolve exists; both reads are of the same main-thread state within one iteration.
pub(crate) fn transport_hidden() -> bool {
    readout_owns_frame(busy())
}

/// Sample the live globals ONCE and resolve the owner. Call this once per frame and pass the result
/// to both [`draw_hud`] and [`draw_readout`] — the same discipline [`slot`] keeps, and for the same
/// reason: two independent derivations of one three-way choice is how the two indicators drifted
/// apart in the first place.
pub(crate) fn busy() -> Busy {
    // dev: `/tmp/plxnative-failtest` forces the failure read-out — the other half of
    // `player::failtest_arm`, which shapes WHICH failure. It is forced HERE, on the one impure
    // sampler, rather than in `player::state()`: the pump acts on that state, and a dev switch
    // that made the engine believe it had failed would be testing a different thing than the
    // screen. `busy_surface` stays pure and ungated, so what draws is still the real rule.
    if crate::dev::flag("failtest") {
        return Busy::Readout(StatusKind::Failed, crate::player::error_caption());
    }
    busy_surface(crate::player::state(), crate::player::seen_frame())
}

// ---- the transport STATE READ-OUT (the glyph slot just past the elapsed clock) ---------------
//
// One slot, one glyph, four states. It is a READ-OUT and not an action toggle — there is no
// transport button row in this app and a standing owner decision forbids adding one, so nothing
// here is focusable, hit-tested or in `BTN_N`. The glyphs are the design system's transport
// family (`Icon::Rewind` / `Icon::Pause` / `Icon::FastForward`, all in one 14-unit band, with
// `Icon::Play` swapping in for `Pause`), which is what keeps the slot from shifting optical
// weight as the state flips under a running clock.

/// How long [`TransportMark::Play`] stands after a resume — "a couple of seconds", per the owner:
/// the mark answers *did that press land*, and a play glyph held for the whole film would be
/// saying "playing" to someone who is watching a moving picture.
const PLAY_MARK_MS: u32 = 2_000;

/// What the state read-out shows this frame. See [`transport_mark`] for the rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TransportMark {
    /// playing steadily — the slot is EMPTY, which is the common case and the point of the whole
    /// design: a mark that is always up says nothing
    None,
    /// paused
    Pause,
    /// the playhead is travelling BACKWARDS (scrub, hop or seek burst)
    Rewind,
    /// the playhead is travelling FORWARDS
    FastForward,
    /// resumed within the last [`PLAY_MARK_MS`]
    Play,
    /// the inline [`Spinner`] — the pipeline is being waited on with NO travel to show
    Working,
}

/// PURE: which mark the state read-out wears, given the transport's live state.
///
/// * `paused` — `player::TX.paused`.
/// * `busy` — the frame's single resolve ([`busy_surface`]); only [`Busy::Transport`] reaches here,
///   the other two being the centred read-out's business.
/// * `scrubbing` — `player::TX.scrub_ns >= 0`, i.e. a LIVE preview the user is dragging. It is its
///   own argument because a preview is not busy: the pipeline is still playing the old position
///   happily while the playhead is dragged around, so `busy` alone cannot see it.
/// * `travel_ns` / `pos_ns` — the DISPLAYED playhead against the PUBLISHED one. Their difference is
///   the direction, and it is deliberately derived from position rather than from the keycode that
///   caused it: one expression then covers a LEFT/RIGHT scrub, a chapter or marker hop and a
///   rapid-seek burst, none of which agree about which key (if any) was pressed.
/// * `since_play_ms` — ms since the last paused→playing edge, `None` if this session has never been
///   resumed. See [`note_transport`], which is where that clock is sampled (NOT here, and not in
///   the draw).
///
/// **Precedence: travel, then the pipeline, then paused, then the resume mark.** Travel outranks
/// `paused` because a paused scrub is still a scrub — that is also the order the slot has always
/// had, when its two states were spinner-over-pause.
///
/// **What happened to the spinner** (the slot's only occupant during a seek until now): it stays,
/// narrowed to what it actually means. `Busy::Transport` is *the pipeline is being waited on with a
/// picture already up*, which is *`Seeking`* — the seek the user asked for — but also *`Buffering`*,
/// a re-buffer nobody asked for. Those are two facts and `player_hud`'s own rule at [`Busy`] is that
/// two indicators for one fact read as two facts; the inverse is just as true, so one indicator for
/// two facts reads as one. The direction glyph therefore takes the slot whenever there IS a
/// direction to show (it says everything the spinner said, plus which way), and the spinner keeps
/// every busy frame that has none — a mid-play re-buffer, and the prime→first-frame tail of a seek
/// after the published position has already caught up with the target. Drawing both was never an
/// option: it is one slot, and they would overlap.
pub(crate) fn transport_mark(
    paused: bool,
    busy: Busy,
    scrubbing: bool,
    travel_ns: i64,
    pos_ns: i64,
    since_play_ms: Option<u32>,
) -> TransportMark {
    let seeking = busy == Busy::Transport;
    if scrubbing || seeking {
        match travel_ns.cmp(&pos_ns) {
            core::cmp::Ordering::Greater => return TransportMark::FastForward,
            core::cmp::Ordering::Less => return TransportMark::Rewind,
            // no direction to show: the seek's target and the published position agree, which is
            // the tail of a landed seek and the whole of a re-buffer. Fall through to the spinner.
            //
            // NB the comparison is against the PLAYHEAD, so it reports NET travel rather than the
            // instantaneous drag: dragging back from +100s to +50s while still ahead of the
            // playhead keeps reading FastForward. That is deliberate and is the more useful of the
            // two — it answers "which way will I jump when I let go", and the alternative flickers
            // the glyph every time a thumb wobbles on the stick.
            core::cmp::Ordering::Equal => {}
        }
    }
    if seeking {
        return TransportMark::Working;
    }
    if paused {
        return TransportMark::Pause;
    }
    match since_play_ms {
        Some(ms) if ms < PLAY_MARK_MS => TransportMark::Play,
        _ => TransportMark::None,
    }
}

/// The paused→playing EDGE, as an `SDL_GetTicks` stamp — the clock behind [`TransportMark::Play`].
/// `None` until this session has been resumed at least once.
///
/// A module static for [`ROW_POP`]'s reason, and stepped from [`note_transport`] for the same one:
/// the HUD keeps no view struct, and this is a CLOCK, which is the trap that doc warns about in its
/// spring form. A stamp taken inside [`draw_hud`] would be taken once per DRAW, and this row is not
/// drawn on every frame of the route (a failure read-out owns the frame; the Info card and the
/// Chapters strip take the transport away) — so a resume pressed with the Info card open would
/// stamp its 2 s from the moment the card CLOSED, or never.
static mut PLAY_AT: Option<u32> = None;
/// Last frame's `TX.paused`, for the edge above. `None` while there is no session, which is what
/// makes the edge per-SESSION: `TX::reset` clears both `started` and `paused` on stop, so without
/// this a session that ended paused would hand the next one a spurious resume edge on its first
/// frame and flash `Play` over a start nobody pressed play for.
static mut PAUSE_SEEN: Option<bool> = None;

/// Step the resume clock — once per frame, from `app.rs`'s update phase, beside [`update`].
fn note_transport(now: u32) {
    let (at, seen) = unsafe { (&mut *std::ptr::addr_of_mut!(PLAY_AT), &mut *std::ptr::addr_of_mut!(PAUSE_SEEN)) };
    if !crate::player::is_started() {
        *at = None;
        *seen = None;
        return;
    }
    let paused = crate::player::TX.paused.load(Relaxed);
    if *seen == Some(true) && !paused {
        *at = Some(now);
    }
    *seen = Some(paused);
}

/// ms since the last resume edge — the read half of [`PLAY_AT`].
///
/// **No `ui::idle` report is owed for this clock**, and that is a finding rather than an omission:
/// `app.rs`'s present gate is `idle::should_present(now) || player`, so the player route presents
/// unconditionally (`system.rs` documents the hardware video plane as slaved to our surface). This
/// is the one place in the app where a raw-time animation cannot ship frozen — every other one
/// (`Xfade::tick`, `Spinner::draw`) had to be taught to report, and both froze first.
fn since_play_ms(now: u32) -> Option<u32> {
    unsafe { *std::ptr::addr_of!(PLAY_AT) }.map(|t| now.wrapping_sub(t))
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
    if kind == StatusKind::Failed {
        // The failure read-out has its own composed layout (`Player Screen.dc.html`, which
        // superseded the retired `Plex Pass Awareness.dc.html` as the spec for this screen)
        // — glyph, fixed verdict, a reason slot, a hint — rather than the shared
        // spinner overlay. The caption is not drawn here: the verdict line is constant by design
        // ("lands at the same y in all three variants, so a user who has seen it once recognises
        // it before reading") and the caption's suffix is re-derived as the reason.
        return draw_failed_readout(Painter::root());
    }
    StatusOverlay::new(readout_frame(), caption, kind).phase(now).draw(&hud_env(), Painter::root());
}

// ---- the failure read-out (`Player Screen.dc.html`) -----------------------------------------
//
// One layout with one optional line. Anchored from the TOP, not centred: the glyph and the
// verdict never move, and the reason sits in a slot that reserves two BODY lines, so a
// zero-, one- or two-element reason leaves the hint at the same y. All on the video ground —
// pure black by the time an error is up — so there is no card chrome.
const FR_GLYPH_S: f32 = 96.0;
const FR_GLYPH_TOP: f32 = 372.0;
const FR_VERDICT_TOP: f32 = FR_GLYPH_TOP + FR_GLYPH_S + 44.0;
const FR_REASON_TOP: f32 = FR_VERDICT_TOP + 48.0 + 24.0;
/// two BODY lines' worth of slot, reserved whether or not anything is in it
const FR_REASON_SLOT: f32 = 84.0;
/// The slot's SECOND line — and there is only one of it, because its two possible occupants are
/// mutually exclusive by construction: the server's quoted verdict belongs to the one arm that
/// never sets `no_pass`, and the Pass line belongs to an arm that carries no verdict. They share
/// the y so a user cannot tell which arm they are looking at by where the line sits.
const FR_SLOT_LINE2: f32 = FR_REASON_TOP + 42.0;
/// The quoted verdict's measure — the mock's `left:340px; right:340px` on a 1920 frame. It exists
/// because the server's sentence is not ours to shorten: given the whole frame a long one runs
/// margin to margin and reads as a paragraph, so it is wrapped to a column instead.
const FR_DETAIL_W: f32 = 1240.0;
const FR_HINT_TOP: f32 = FR_REASON_TOP + FR_REASON_SLOT + 56.0;

/// One line of centred text with its cap TOP at `top`; returns nothing — the layout is fixed.
fn fr_line(p: Painter, text: &std::ffi::CStr, top: f32, sz: i32, bold: i32, col: [f32; 4]) {
    let (cap_top, _) = crate::text::text_cap_band(sz, bold);
    let w = crate::text::text_width(text.as_ptr(), sz, bold);
    p.text(text.as_ptr(), (SCR_W - w) * 0.5, top - cap_top, sz, col, 0, bold);
}

fn draw_failed_readout(p: Painter) {
    let e = crate::player::error_now();
    // The GROUND, first: `Player Screen.dc.html` gives the failed variant `inset:0; background:#000`
    // — a full-bleed opaque black — and it is one quad. Without it this layout stood on whatever the
    // video plane happened to be holding: `app.rs` clears the graphics plane to alpha 0 on the player
    // route, and the transport (whose bottom scrim used to back the lower half of this read-out) is
    // no longer drawn at all under a failure. "The plane is black by the time an error is up" is
    // usually true and not always — `busy_surface` resolves Error to Failed for BOTH values of
    // `seen_frame`, i.e. a mid-playback failure over a held frame is a state the code contemplates.
    p.rrect(Rect::FULL, 0.0, 0.0, theme::scrim_black(1.0));
    // glyph: the same triangle as the facts row at 96, secondary ink — outline, because a solid
    // triangle at this size reads as an error state we do not have (the verdict is the words)
    let gx = (SCR_W - FR_GLYPH_S) * 0.5;
    crate::ui::icons::draw(p, crate::ui::icons::Icon::Alert, Rect::new(gx, FR_GLYPH_TOP, FR_GLYPH_S, FR_GLYPH_S), theme::TEXT_SECONDARY);
    fr_line(p, c"Playback failed", FR_VERDICT_TOP, theme::size::TITLE, 1, theme::TEXT_PRIMARY);
    // the reason slot: line one is the reason; line two is EITHER the server's own sentence (a
    // `/decision` refusal) or — only ever on a known-free server — the subscription FACT. Never
    // both: see `FR_SLOT_LINE2`.
    if !e.readout.is_empty() {
        if let Ok(c) = std::ffi::CString::new(e.readout) {
            fr_line(p, &c, FR_REASON_TOP, theme::size::BODY, 0, theme::TEXT_SECONDARY);
        }
    }
    if !e.detail.is_empty() {
        // The SERVER's sentence, quoted verbatim at CAPTION/tertiary: quieter than the reason above
        // it because it is supporting evidence, and a size below it because it is the only line here
        // whose length we do not control. `max_lines(2)` is the honest clamp — one wrap keeps the
        // whole of a real verdict (the cause is at its END: "…encoder 'vp9' not found"), where a
        // one-line elide would cut exactly the words worth reading. A second line paints past the
        // reserved slot into the gap above the hint, which is paint, not layout: the hint's y is a
        // constant and does not move.
        crate::ui::text_view::TextView::new(&e.detail, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .h(crate::ui::label::HAlign::Center)
            .max_lines(2)
            .draw(p, Rect::new((SCR_W - FR_DETAIL_W) * 0.5, FR_SLOT_LINE2, FR_DETAIL_W, 0.0));
    }
    if e.no_pass {
        let words = c"This server has no";
        let ww = crate::text::text_width(words.as_ptr(), theme::size::BODY, 0);
        let cw = crate::ui::widgets::pass_capsule_w();
        const GAP: f32 = 16.0;
        let x = (SCR_W - (ww + GAP + cw)) * 0.5;
        let line_top = FR_SLOT_LINE2; // the slot's second line — shared with the quoted verdict
        let (cap_top, baseline) = crate::text::text_cap_band(theme::size::BODY, 0);
        p.text(words.as_ptr(), x, line_top - cap_top, theme::size::BODY, theme::TEXT_SECONDARY, 0, 0);
        let cy = line_top + (baseline - cap_top) * 0.5;
        crate::ui::widgets::pass_capsule(p, x + ww + GAP, cy, true);
    }
    // the hint, with BACK as a key cap — the cap is what survives a phone photo
    draw_hint_with_keycap(p, c"Press", c"BACK", c"to return", FR_HINT_TOP);
}

/// "{pre} [KEY] {post}", centred at cap-top `top` — CAPTION tertiary prose around a keyline cap
/// (min-w 74, h 36, r 8), the cap's label MICRO bold. The keyline is a knockout on the video
/// ground, which is black here by construction.
fn draw_hint_with_keycap(p: Painter, pre: &std::ffi::CStr, key: &std::ffi::CStr, post: &std::ffi::CStr, top: f32) {
    const CAP_H: f32 = 36.0;
    const CAP_MIN_W: f32 = 74.0;
    const CAP_PAD: f32 = 12.0;
    const GAP: f32 = 14.0;
    let sz = theme::size::CAPTION;
    let pw = crate::text::text_width(pre.as_ptr(), sz, 0);
    let ow = crate::text::text_width(post.as_ptr(), sz, 0);
    let kw = (crate::text::text_width(key.as_ptr(), theme::size::MICRO, 1) + 2.0 * CAP_PAD).max(CAP_MIN_W);
    let total = pw + GAP + kw + GAP + ow;
    let x = (SCR_W - total) * 0.5;
    let (cap_top, baseline) = crate::text::text_cap_band(sz, 0);
    let ty = top - cap_top;
    let cy = top + (baseline - cap_top) * 0.5;
    p.text(pre.as_ptr(), x, ty, sz, theme::TEXT_TERTIARY, 0, 0);
    let kx = x + pw + GAP;
    let kr = Rect::new(kx, cy - CAP_H * 0.5, kw, CAP_H);
    const STROKE: f32 = 1.5;
    p.rrect(kr, 8.0, 8.0, [1.0, 1.0, 1.0, 0.34]);
    p.rrect(Rect::new(kr.x + STROKE, kr.y + STROKE, kr.w - 2.0 * STROKE, kr.h - 2.0 * STROKE), 8.0 - STROKE, 8.0 - STROKE, [0.0, 0.0, 0.0, 1.0]);
    let kty = crate::text::text_vcenter_y(theme::size::MICRO, 1, cy);
    let ktw = crate::text::text_width(key.as_ptr(), theme::size::MICRO, 1);
    p.text(key.as_ptr(), kx + (kw - ktw) * 0.5, kty, theme::size::MICRO, theme::TEXT_SECONDARY, 0, 1);
    p.text(post.as_ptr(), kx + kw + GAP, ty, sz, theme::TEXT_TERTIARY, 0, 0);
}

/// x of control button `idx`, left to right: 0 = Subtitles, 1 = Audio, 2 = More.
///
/// The row is RIGHT-anchored — the LAST disc's edge is `CTRL_RIGHT` — so adding a button pushes
/// the existing ones left instead of moving the row's right margin, which is what keeps the discs
/// and every stand-in (`ctrl_slot`) sharing one edge.
fn btn_x(idx: i32) -> f32 {
    CTRL_RIGHT - BTN_S - (BTN_N - 1 - idx) as f32 * (BTN_S + BTN_GAP)
}

/// which control button a pointer at (cx,cy) is over: 0 = Subtitles, 1 = Audio, 2 = More, or None.
/// The single source of truth for the button rects, shared with draw_hud.
///
/// `slot` is passed in rather than re-derived: while a stand-in owns the row the discs are not on
/// screen, and a click in that band must not open a track menu the user cannot see.
pub(crate) fn icon_hit(slot: ControlSlot, cx: f32, cy: f32) -> Option<i32> {
    if !slot.is_discs() || cy < BTN_Y || cy > BTN_Y + BTN_S {
        return None;
    }
    (0..BTN_N).find(|&idx| {
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

// ---- the rail carries NO marks ------------------------------------------------------------
// Nothing is drawn on the scrubber but the track, the watched fill and the playhead. Two kinds of
// mark have been tried here and both were removed, for the same reason and by the same judgement:
//
//   * chapter-boundary ticks — the rail reads cleaner as one continuous bar. The chapter LIST is
//     the affordance (`ui/chapters_panel.rs`, off `metadata::playing_chapters()`).
//   * intro/credits segment bands — removed 2026-08-04. They were `RAIL_MARKER`, white at 0.42
//     against a 0.20 track, so a credits band on a feature film was a bright unlabelled patch
//     floating near the right end of an otherwise empty rail. Reviewed cold on a screenshot it
//     read as a RENDERING ARTIFACT, not as information, which is a complete failure of the thing.
//
// Neither loses anything: the Skip Intro / Skip Credits pill (`ui/skip_pill.rs`) is driven from
// the very same `metadata::playing_markers()` and appears exactly when a marker is reachable, so
// the band was decoration duplicating a control that already announces itself. Do not re-add
// either as a "cheap win" — the marker data is already in memory, which is precisely what makes
// drawing it tempting and still wrong.

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
    // A FAILURE owns the frame, and it outranks every branch below — including the Up Next card,
    // which cannot coexist with one but must not be the arm that decides so. `Player Screen.dc.html`
    // sets `hudDisplay:none` AND `tabsDisplay:none` on the failed variant, and its read-out block
    // says why in one line: "the transport is not drawn: with no picture and no position a
    // live-looking scrubber at 0:00 is the historical bug this read-out exists to end."
    //
    // Without this the bug was only half-fixed. `Error` is not `is_busy()`, so it never pins the
    // HUD — but it does not HIDE it either, so for the ~4.5 s of `app.rs`'s linger after entry the
    // scrubber, clocks and tabs drew live at 0:00 UNDER the read-out, and only then went away. The
    // read-out itself already outlives the linger (`draw_readout` is not transport chrome), so this
    // is the missing half of that same rule rather than a new one.
    //
    // Gated on the resolved `busy` and not on `player::state()`: [`busy_surface`] is the ONE place
    // the surfaces are divided, and re-reading the state here is exactly the second derivation its
    // doc exists to forbid. Deliberately NOT gated on `transport` — the Info card / Chapters strip
    // pass false and would otherwise put the bottom scrim + tabs back under a failure.
    if readout_owns_frame(busy) {
        return;
    }
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
        ControlSlot::UpNext(_) => crate::ui::up_next::draw(p, focus == 1, btn, now),
        ControlSlot::Skip(pr) => crate::ui::skip_pill::draw(p, pr, focus == 1),
        ControlSlot::Discs => {
            for i in 0..BTN_N {
                TransportButton::new(i, Rect::new(btn_x(i), BTN_Y, BTN_S, BTN_S))
                    .focused(focus == 1 && btn == i)
                    // The whole control row stands on the VIDEO PLANE — see `ControlGround`. It is
                    // the HUD's own bottom ramp (drawn at the top of this function) that makes the
                    // unkeyed face legal: the picture is unreadable, but the ground under these
                    // discs is known to be dark.
                    .ground(ControlGround::Unkeyed)
                    .scale(row_pop(i))
                    .draw(&e, p);
            }
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
    // Hoisted so the display position and the PUBLISHED one are one sample: the state read-out
    // below takes its travel direction from the difference between them, and two loads of a live
    // atomic can straddle a tick.
    let livepos = crate::player::playpos_ns();
    // ONE sample of the seek target too, for the reason the line above hoists the playhead: the
    // condition and the value were two loads of the same live atomic and could straddle a tick.
    let seekdisp = crate::player::seek_display_ns();
    let dispos = if loading && seekdisp >= 0 {
        seekdisp
    } else if scrub >= 0 {
        scrub
    } else {
        livepos
    };
    let dur = crate::player::duration_ns();
    let frac = if dur > 0 { (dispos as f64 / dur as f64).clamp(0.0, 1.0) } else { 0.0 };
    p.rect(Rect::new(sx, sy, sw, sh), sh * 0.5, track, track, 0.0);
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
    // transport state indicator just past the elapsed clock — the four-state READ-OUT resolved by
    // [`transport_mark`] (rewind / pause / fast-forward / play, plus the narrowed spinner), and
    // NOTHING while playing steadily. A read-out, not an action toggle: nothing here is focusable
    // or hit-tested, and the control row stays the three discs it has always been.
    // Gated on `busy`, not on `loading()`: with `loading()` the transport lit the same spinner the
    // centred read-out was already showing, for the whole of every load AND every seek. Centered on
    // the clock's line box; drops to the clock's LEFT when the right side is against the remaining
    // label / screen edge.
    let paused = crate::player::TX.paused.load(Relaxed);
    let mark = transport_mark(paused, busy, scrub >= 0, dispos, livepos, since_play_ms(now));
    if mark != TransportMark::None {
        // pause bars under-fill their viewBox (14/24 tall) — a 30px box renders ~17px of ink,
        // matching the CAPTION clock's cap height so the glyph reads as the label's size. The two
        // travel marks are drawn to that SAME 14-unit band (see `Icon::Rewind`), which is what lets
        // one box serve the whole family without the slot changing weight as the state flips.
        let isz = 30.0f32;
        // The odd member of the family — `play.svg` is authored to 16 units where the other three
        // are 14 — is corrected by asking `icons::band` rather than by a constant here. See that
        // function: the metric is a property of the asset, and this was the second screen to
        // transcribe one out of an SVG by hand.
        // **The gap is measured to the INK, not to the box.** Every member of this family carries a
        // different left bearing inside its 24-unit viewBox — pause's bars open at x=7, play's
        // triangle at x=6, the travel marks at x=2.6 — so ONE box origin gives each state a
        // visibly different gap after the clock, and the slot appears to twitch as the state flips.
        // Measuring to the ink makes the gap the eye sees a single number. It is also what the old
        // spacing really was: a box placed 14px out put pause's ink at 14 + 7/24*30 ~= 23px, which
        // is the gap that read as too wide. The bearings come from `icons::ink_x`, not from a table
        // here — they are the asset's, and this screen was the second place to copy them out.
        const GAP: f32 = 14.0;
        let glyph = match mark {
            TransportMark::Pause => Some(crate::ui::icons::Icon::Pause),
            TransportMark::Play => Some(crate::ui::icons::Icon::Play),
            TransportMark::Rewind => Some(crate::ui::icons::Icon::Rewind),
            TransportMark::FastForward => Some(crate::ui::icons::Icon::FastForward),
            TransportMark::Working | TransportMark::None => None,
        };
        let ink = glyph.map_or((0.0, 1.0), crate::ui::icons::ink_x);
        let icy = ty + crate::text::text_height(theme::size::CAPTION, 1) * 0.5; // vertical center of the clock line
        // scaled so every member of the family lands the SAME height of ink in this one box
        let bs = glyph.map_or(isz, |g| {
            isz * crate::ui::icons::band(crate::ui::icons::Icon::Pause) / crate::ui::icons::band(g)
        });
        // **Rewind sits to the LEFT of the clock; everything else to the right.** The mark points
        // the way the playhead is travelling, so `<<` after the time would point back at the number
        // it is leaving. The right-hand placement still falls back to the left when the remaining
        // label or the screen edge crowds it, which is the case this branch was originally for.
        let need = (ink.1 - ink.0) * bs + 6.0;
        let room_right = el_r + GAP + need < if rem_shown { rem_l - 8.0 } else { sx + sw };
        let on_left = mark == TransportMark::Rewind || !room_right;
        // Placed by ink on whichever side it lands: the trailing edge sits GAP before the clock's
        // left, or the leading edge GAP after the clock's right.
        let bx = if on_left { el_l - GAP - ink.1 * bs } else { el_r + GAP - ink.0 * bs };
        match glyph {
            Some(id) => crate::ui::icons::draw(p, id, Rect::new(bx, icy - bs * 0.5, bs, bs), white),
            None => Spinner::new(bx + isz * 0.5, icy, Spinner::R_INLINE).phase(now).tint(white).draw(&e, p),
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
            TabPill::new(cs.as_ptr(), theme::size::BODY, Rect::new(px, py, pw, ph))
                .focused(on)
                // Same row band, same ramp, same ground as the transport discs — everything the
                // HUD draws stands on the video plane (`ControlGround`).
                .ground(ControlGround::Unkeyed)
                .draw(&e, p);
        }
        px += pw + 16.0;
    }
}

/// **The transport's outermost drawn chrome, for the overscan audit**
/// ([`crate::ui::consts::SAFE`]). Private geometry, so the rects are built where they are drawn
/// rather than restated in the module that grades them.
///
/// The bottom tab row is measured at its widest — `Info` + `Chapters`, the pair an item with
/// chapters draws — because a pill's width is the label's and the audit has to grade the widest
/// state a screen can be in, not the one a screenshot happened to catch. That measurement needs a
/// font, which the host suite cannot link, so it is bounded by the row's OWN height instead: what
/// the audit is about here is the row's left edge and its BOTTOM, and both are font-independent.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    out.push(("player scrubber", Rect::new(SB_X, SB_Y, sb_w(), SB_H)));
    out.push(("player control row", Rect::new(CTRL_RIGHT - CTRL_ROW_W, CTRL_Y, CTRL_ROW_W, CTRL_H)));
    let py = (SB_Y + SCR_H) * 0.5 - BTN_S * 0.5;
    out.push(("player bottom tab row", Rect::new(SB_X, py, 0.0, BTN_S)));
    out.push(("player title / elapsed clock", Rect::new(SB_X, SCR_H - 312.0, 0.0, 0.0)));
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

    /// The disc row's geometry and its hit-test are ONE definition (`icon_hit` is documented as the
    /// single source of the rects, shared with the draw), so the property worth asserting is that
    /// they agree: every disc's own centre must hit-test back to that disc. Added when the row grew
    /// a third disc — the `…` overflow — because the previous shape hard-coded two positions and a
    /// `(0..2)` scan, and a third button that draws but cannot be clicked is exactly the kind of
    /// half-wired control this file's `ControlSlot` note exists to prevent.
    #[test]
    fn every_disc_hit_tests_back_to_itself() {
        let cy = BTN_Y + BTN_S * 0.5;
        for i in 0..BTN_N {
            let cx = btn_x(i) + BTN_S * 0.5;
            assert_eq!(icon_hit(ControlSlot::Discs, cx, cy), Some(i), "disc {i} centre");
        }
    }

    /// The row is right-anchored and does not overlap itself: the LAST disc ends exactly at
    /// `CTRL_RIGHT`, each disc sits a full `BTN_GAP` clear of its neighbour, and the whole span is
    /// `CTRL_ROW_W` — which is the floor `ctrl_slot` gives a stand-in, so a drifting span would
    /// silently let the Skip pill shrink the row.
    #[test]
    fn the_disc_row_is_right_anchored_and_spans_ctrl_row_w() {
        assert_eq!(btn_x(BTN_N - 1) + BTN_S, CTRL_RIGHT, "last disc must end at the row's edge");
        for i in 1..BTN_N {
            assert_eq!(btn_x(i) - (btn_x(i - 1) + BTN_S), BTN_GAP, "gap before disc {i}");
        }
        assert_eq!(CTRL_RIGHT - btn_x(0), CTRL_ROW_W, "drawn span vs the stand-in floor");
    }

    /// A point in the row's band but in a GAP, or past either end, belongs to no disc — the click
    /// path falls through to the scrubber there, so a gap that answered `Some` would open a track
    /// menu from a press on empty chrome.
    #[test]
    fn gaps_and_the_row_ends_belong_to_no_disc() {
        let cy = BTN_Y + BTN_S * 0.5;
        assert_eq!(icon_hit(ControlSlot::Discs, btn_x(0) - 1.0, cy), None, "left of the row");
        assert_eq!(icon_hit(ControlSlot::Discs, CTRL_RIGHT + 1.0, cy), None, "right of the row");
        for i in 1..BTN_N {
            let mid = btn_x(i) - BTN_GAP * 0.5;
            assert_eq!(icon_hit(ControlSlot::Discs, mid, cy), None, "gap before disc {i}");
        }
        // and the band itself is bounded vertically
        let cx = btn_x(0) + BTN_S * 0.5;
        assert_eq!(icon_hit(ControlSlot::Discs, cx, BTN_Y - 1.0), None, "above the row");
        assert_eq!(icon_hit(ControlSlot::Discs, cx, BTN_Y + BTN_S + 1.0), None, "below the row");
    }

    /// While a stand-in owns the row the discs are not on screen, so nothing in that band may
    /// report a disc — the rule `icon_hit`'s `slot` parameter exists for.
    #[test]
    fn a_stand_in_owning_the_row_hides_every_disc_from_the_hit_test() {
        let cy = BTN_Y + BTN_S * 0.5;
        let slot = slot_for(Some(marker(MarkerKind::Intro, false)), false);
        for i in 0..BTN_N {
            assert_eq!(icon_hit(slot, btn_x(i) + BTN_S * 0.5, cy), None, "disc {i} under a stand-in");
        }
    }

    /// The control row's precedence, which used to live in the ORDER of five separate if-chains
    /// across `player_hud` and `app.rs` — the draw, the OK handler, the pointer handler, the
    /// LEFT/RIGHT clamp and `icon_hit` — with nothing keeping them in step and nothing to test.
    #[test]
    fn the_control_row_picks_the_most_specific_occupant() {
        // no segment under the playhead → the ordinary Subtitles + Audio pair
        assert!(slot_for(None, false).is_discs());
        assert!(slot_for(None, true).is_discs(), "a queued successor alone changes nothing");
        assert_eq!(slot_for(None, true).items(), BTN_N);
        assert_eq!(slot_for(None, true).primary_btn(), 0, "the discs open on Subtitles");

        // an intro is always Skip, successor or not — "what's next" is an end-of-episode idea
        for has_next in [false, true] {
            let slot = slot_for(Some(marker(MarkerKind::Intro, false)), has_next);
            assert!(matches!(slot, ControlSlot::Skip(p) if p.kind == MarkerKind::Intro));
            assert_eq!(slot.items(), 1, "a Skip pill is the row's only item");
            assert_eq!(slot.primary_btn(), 0, "…so it is also the one focus lands on");
        }

        // credits WITH somewhere to go → Up Next outranks Skip Credits…
        let up = slot_for(Some(marker(MarkerKind::Credits, true)), true);
        assert!(matches!(up, ControlSlot::UpNext(_)));
        // …and it is the ONE stand-in with a pair, whose primary is the RIGHT-hand item. Asserted
        // together because they are one fact: an `items()` of 2 with a `primary_btn()` of 0 would
        // park the ring on Watch Credits, and `up_next::countdown_may_run` would then disarm the
        // countdown on the frame after it armed — a timer that visibly never runs.
        assert_eq!(up.items(), 2, "Watch Credits + Next Episode");
        assert_eq!(up.primary_btn(), crate::ui::up_next::BTN_NEXT);
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

    /// …and when it does, it owns the WHOLE frame: `draw_hud` draws nothing — no scrim, no
    /// scrubber, no clocks, no bottom tabs (`Player Screen.dc.html` sets `hudDisplay:none` and
    /// `tabsDisplay:none` on the failed variant, because a live-looking scrubber at 0:00 over a
    /// black panel is the bug the read-out exists to end).
    ///
    /// The other half is the one that would be easy to break by simplifying this to "a read-out is
    /// up": every WORKING read-out must leave the transport alone, or the HUD blanks through every
    /// cold start, every reconnect and every pre-roll seek — states where the position is real and
    /// the transport is what the user reads the instant the first frame lands.
    #[test]
    fn only_a_failure_takes_the_frame_away_from_the_transport() {
        for seen in [false, true] {
            assert!(readout_owns_frame(busy_surface(S::Error, seen)), "a failure hides the transport");
        }
        for st in [S::Resolving, S::Connecting, S::Buffering, S::Seeking] {
            let b = busy_surface(st, false);
            assert!(matches!(b, Busy::Readout(StatusKind::Working, _)), "{st:?} is a read-out");
            assert!(!readout_owns_frame(b), "{st:?} is mid-flight — the transport stays up under it");
        }
        // and the two non-read-out surfaces are never a reason to blank the frame
        assert!(!readout_owns_frame(busy_surface(S::Playing, true)));
        assert!(!readout_owns_frame(busy_surface(S::Seeking, true)), "a seek over a live picture");
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

    // ---- the transport state read-out ---------------------------------------------------------
    //
    // `transport_mark` is the whole rule, factored out of the draw so it can be graded here: the
    // draw itself is one `match` over the returned value, and the slot's geometry is unchanged.
    // 1 s in ns, so a position reads as a second.
    const S: i64 = 1_000_000_000;

    /// The common case, and the one the design is FOR: frames on the panel, nothing pressed
    /// recently, and the slot is empty. A mark that is always up says nothing.
    #[test]
    fn playing_steadily_shows_nothing() {
        assert_eq!(
            transport_mark(false, Busy::None, false, 42 * S, 42 * S, None),
            TransportMark::None
        );
        // …and the resume mark has expired rather than never existed
        assert_eq!(
            transport_mark(false, Busy::None, false, 42 * S, 42 * S, Some(PLAY_MARK_MS)),
            TransportMark::None
        );
    }

    /// Paused is UNCHANGED from the behaviour that shipped before the read-out grew its other two
    /// states, and it outranks the resume mark: a user who resumed and re-paused inside two seconds
    /// is paused, whatever the clock still says.
    #[test]
    fn paused_shows_pause_and_outranks_a_live_resume_clock() {
        assert_eq!(transport_mark(true, Busy::None, false, 42 * S, 42 * S, None), TransportMark::Pause);
        assert_eq!(
            transport_mark(true, Busy::None, false, 42 * S, 42 * S, Some(200)),
            TransportMark::Pause
        );
    }

    /// Direction comes from the POSITIONS, not from a keycode — which is what makes one expression
    /// cover the three ways the playhead travels. A live scrub is not busy (the pipeline is still
    /// playing the old position), so `scrubbing` has to be its own input.
    #[test]
    fn travel_direction_is_read_off_the_positions() {
        // LEFT/RIGHT scrub preview, while playing: not busy at all
        assert_eq!(
            transport_mark(false, Busy::None, true, 52 * S, 42 * S, None),
            TransportMark::FastForward
        );
        assert_eq!(transport_mark(false, Busy::None, true, 32 * S, 42 * S, None), TransportMark::Rewind);
        // a chapter/marker hop or a rapid-seek burst: scrub cleared, the seek in flight, the frozen
        // playhead sitting at the target while the published position is still where we left
        assert_eq!(
            transport_mark(false, Busy::Transport, false, 600 * S, 42 * S, None),
            TransportMark::FastForward
        );
        assert_eq!(
            transport_mark(false, Busy::Transport, false, 5 * S, 42 * S, None),
            TransportMark::Rewind
        );
    }

    /// Travel outranks paused — a paused scrub is still a scrub, and that is also the precedence
    /// the slot had when its two states were spinner-over-pause.
    #[test]
    fn a_paused_scrub_still_reads_as_travel() {
        assert_eq!(transport_mark(true, Busy::None, true, 32 * S, 42 * S, None), TransportMark::Rewind);
        assert_eq!(
            transport_mark(true, Busy::Transport, false, 90 * S, 42 * S, None),
            TransportMark::FastForward
        );
    }

    /// What the spinner was narrowed TO. `Busy::Transport` is two facts — the seek the user asked
    /// for, and a re-buffer nobody asked for — and the direction glyph can only speak for the
    /// first. So the spinner keeps every busy frame with no direction in it, and gives up the ones
    /// that have one.
    #[test]
    fn the_spinner_keeps_only_the_busy_frames_with_no_direction() {
        // a mid-play re-buffer: busy, but the playhead is not going anywhere
        assert_eq!(
            transport_mark(false, Busy::Transport, false, 42 * S, 42 * S, None),
            TransportMark::Working
        );
        // the prime→first-frame tail of a landed seek, once the published position has caught up
        assert_eq!(
            transport_mark(false, Busy::Transport, true, 42 * S, 42 * S, None),
            TransportMark::Working
        );
        // and the centred read-out's own states never reach this slot
        assert_eq!(
            transport_mark(false, Busy::Readout(StatusKind::Working, c"Buffering…"), false, 0, 0, None),
            TransportMark::None
        );
    }

    /// "Play sign show only for a couple of seconds when we press play, do not show it all the play
    /// time." The clock is ms since the paused→playing edge; `None` means this session has never
    /// been resumed, which is a fresh start rather than a resume and draws nothing.
    #[test]
    fn the_play_mark_expires_and_a_never_resumed_session_has_none() {
        assert_eq!(transport_mark(false, Busy::None, false, 0, 0, Some(0)), TransportMark::Play);
        assert_eq!(
            transport_mark(false, Busy::None, false, 0, 0, Some(PLAY_MARK_MS - 1)),
            TransportMark::Play
        );
        assert_eq!(
            transport_mark(false, Busy::None, false, 0, 0, Some(PLAY_MARK_MS)),
            TransportMark::None,
            "the couple of seconds is a boundary, not a suggestion"
        );
        assert_eq!(transport_mark(false, Busy::None, false, 0, 0, None), TransportMark::None);
    }

    /// A resume the user asked for while the playhead is also travelling: the travel is the newer
    /// fact and the slot is one glyph, so it wins. (This is the frame after a paused scrub commits
    /// — `commit_seek` drops `paused` to let the pipeline prime the new position.)
    #[test]
    fn travel_outranks_the_resume_mark() {
        assert_eq!(
            transport_mark(false, Busy::Transport, false, 90 * S, 42 * S, Some(0)),
            TransportMark::FastForward
        );
    }
}
