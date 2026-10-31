//! **The scrub preview** — a still from the scrub position, floated above the playbar's knob while
//! the user scrubs (`docs/parity-gaps.md`, "No BIF thumbnail preview while scrubbing").
//!
//! **Where the stills come from.** A server that generates video preview thumbnails keeps a BIF
//! index per part, and PMS serves single stills out of it at
//! `/library/parts/{part}/indexes/sd/{ms}` — about 10 KB of 320×180 JPEG each, the nearest frame
//! to `ms`. The whole BIF is tens of MB against a 32-bit heap, so it is never fetched: each still
//! is ordinary keyed art through the poster source and the render cache (`plx_ui::tex`), exactly
//! like a chapter card's thumbnail. `plx_platform::imgcache` keeps them off the disk tier.
//!
//! **Absence is the ordinary case.** Many libraries have preview thumbnails off, and then every
//! still is a 404. The first request of a playback that settles as a miss before any still has
//! arrived marks the part as having no index, and nothing is drawn or asked for again until the
//! next playback — no placeholder box, no request per scrub step.
//!
//! **One request in flight, always for the newest position.** A held scrub crosses a film in
//! seconds. Asking for every step it passes would fill the source's 64 slots with stills nobody
//! will see; instead the memo asks for the position under the knob, waits for that one to settle,
//! and then asks for wherever the knob has got to. Until a new still lands, the last one stays up.
//!
//! The memo is a RENDER memo, like the rest of [`crate::player_hud::motion::HudMotion`]'s render state: what it
//! holds is which still the draw can show, and it is stepped where the art is probed — in the draw.

use plx_ui::{theme, Painter, Rect};

/// The stills' request size: the BIF's own 320×180, so the server's transcoder never upscales.
pub const REQ_W: i32 = 320;
pub const REQ_H: i32 = 180;
/// The drawn card's height, fixed; its width follows the still's own aspect ([`card_size`]), so a
/// 16:9 still is 384 wide.
pub const CARD_H: f32 = 216.0;
/// The widest the card grows: a 2.40:1 still (518×216). Anything wider is letterboxed inside it.
pub const CARD_W_CAP: f32 = 518.0;
/// The narrowest: a square. A portrait still is letterboxed inside it rather than a sliver.
const CARD_W_FLOOR: f32 = CARD_H;
pub const CARD_RAD: f32 = 12.0;
/// The inside rim's stroke width (colour: [`theme::CARD_SHEEN`], white 22 %).
pub const CARD_RIM_W: f32 = 1.5;
/// The drop shadow: penumbra and downward offset (colour: [`theme::scrim_black`] at
/// [`CARD_SHADOW_A`]).
pub const CARD_SHADOW_BLUR: f32 = 32.0;
pub const CARD_SHADOW_DY: f32 = 12.0;
pub const CARD_SHADOW_A: f32 = 0.5;
/// Clearance between the card's bottom edge and the scrubber's top.
const CARD_GAP: f32 = 26.0;
/// A request that has neither arrived nor failed by now is given up on, so a declined or lost one
/// cannot freeze the preview on an old still for the rest of the scrub.
const PENDING_MAX_MS: u32 = 1500;

/// What a probe of one still found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    /// resident: drawable this frame
    Ready,
    /// settled as a miss (a 404 — this part has no index, or none at this offset)
    Failed,
    /// requested, in flight, or not admitted yet
    Waiting,
}

/// **Where the card's stills come from.** The memo ([`ScrubPreview::step`]) and the draw ask a
/// source for two things about a still, named by its ms: whether it can be drawn
/// ([`Stills::look`], which also STARTS the fetch of one never asked for) and the texture to draw
/// ([`Stills::tex`]). [`PosterStills`] is the PMS source above; a source that yields one whole
/// texture per still implements this and draws through the same card, memo and motion, rather
/// than a card of its own. The draw samples the whole texture `tex` names, so a still that is a
/// region of a larger texture is outside this seam.
pub trait Stills {
    /// Probe the still at `ms`: resident, settled as a miss, or still on its way.
    fn look(&mut self, ms: i64) -> Look;
    /// The still at `ms` as `(texture, width, height)` in decoded pixels, or `None` while it is
    /// not resident. A source that holds one still at a time answers for that one only: the draw
    /// then swaps without a crossfade.
    fn tex(&mut self, ms: i64) -> Option<(u32, f32, f32)>;
}

/// The part's own stills: ordinary keyed art through the poster source and the render cache.
pub struct PosterStills {
    /// the playing item's server, as the poster source names it
    pub sid: u16,
    pub part: i64,
}

impl Stills for PosterStills {
    fn look(&mut self, ms: i64) -> Look {
        let path = still_path(self.part, ms);
        if plx_ui::tex::resolve_on(self.sid, &path, REQ_W, REQ_H, false) != 0 {
            Look::Ready
        } else if plx_ui::tex::failed_on(self.sid, &path, REQ_W, REQ_H, false) {
            Look::Failed
        } else {
            Look::Waiting
        }
    }

    fn tex(&mut self, ms: i64) -> Option<(u32, f32, f32)> {
        let (tex, w, h) = plx_ui::tex::resolve_wh_on(self.sid, &still_path(self.part, ms), REQ_W, REQ_H, false);
        (tex != 0 && w > 0.0 && h > 0.0).then_some((tex, w, h))
    }
}

/// The scrub preview's memo. Owned by the player's [`crate::player_hud::motion::HudMotion`].
#[derive(Default)]
pub struct ScrubPreview {
    /// The playback the memo describes: `(part id, playback generation)`. Any change resets it, so
    /// a part whose thumbnails were generated since is asked again on its next playback.
    owner: Option<(i64, String)>,
    /// This playback's part has no index: draw nothing, ask nothing.
    absent: bool,
    /// The still (ms) last seen resident.
    shown: Option<i64>,
    /// The one request in flight: `(ms, asked at)`.
    pending: Option<(i64, u32)>,
    /// The scrub's motion: what fades, when ([`Motion`]).
    pub motion: Motion,
}

/// The still a scrub at `pos_ns` asks for: the knob's own position, to the nearest second. PMS
/// answers with its NEAREST still (one per `GenerateBIFFrameInterval`, from the nearest keyframe),
/// so rounding here would only add error on top of the index's own spacing. The second is enough
/// to keep a resting knob on one key; how many keys a held scrub crosses is bounded by the one
/// request in flight, not by this.
pub fn still_ms(pos_ns: i64) -> i64 {
    (pos_ns.max(0) + 500_000_000) / 1_000_000_000 * 1_000
}

/// The still's server path — what the poster source turns into a transcode request.
pub fn still_path(part: i64, ms: i64) -> String {
    format!("/library/parts/{part}/indexes/sd/{ms}")
}

impl ScrubPreview {
    pub const fn new() -> Self {
        Self { owner: None, absent: false, shown: None, pending: None, motion: Motion::new() }
    }

    /// Step the memo for one frame and answer which still to draw (`None`: draw nothing).
    ///
    /// `look` probes one still by its ms — and a probe of a still never asked for STARTS its
    /// fetch, which is why this function decides which stills get probed at all: the pending one,
    /// then the target only once nothing is in flight.
    pub fn step(
        &mut self,
        owner: (i64, &str),
        target_ms: i64,
        now: u32,
        mut look: impl FnMut(i64) -> Look,
    ) -> Option<i64> {
        if owner.0 <= 0 {
            return None;
        }
        if self.owner.as_ref().is_none_or(|(p, g)| *p != owner.0 || g != owner.1) {
            *self = Self { owner: Some((owner.0, owner.1.to_owned())), motion: self.motion, ..Self::new() };
        }
        if self.absent {
            return None;
        }
        if let Some((ms, at)) = self.pending {
            match look(ms) {
                Look::Ready => {
                    self.shown = Some(ms);
                    self.pending = None;
                }
                Look::Failed => {
                    self.pending = None;
                    if self.shown.is_none() {
                        self.absent = true;
                        return None;
                    }
                }
                Look::Waiting => {
                    if now.wrapping_sub(at) > PENDING_MAX_MS {
                        self.pending = None;
                    }
                }
            }
        }
        if self.pending.is_none() && self.shown != Some(target_ms) {
            match look(target_ms) {
                Look::Ready => self.shown = Some(target_ms),
                Look::Failed => {
                    if self.shown.is_none() {
                        self.absent = true;
                        return None;
                    }
                }
                Look::Waiting => self.pending = Some((target_ms, now)),
            }
        }
        self.shown
    }

    /// The scrub ended: forget its stills, so the next one does not open on this one's last
    /// picture at a different position. What is known about the part (`absent`) stays.
    pub fn release(&mut self) {
        self.shown = None;
        self.pending = None;
    }

    /// This playback's part has no index (a 404 before any still arrived).
    pub fn absent(&self) -> bool {
        self.absent
    }
}

/// The card's size for a still of `tw`×`th` pixels: [`CARD_H`] tall, as wide as the still's own
/// aspect makes it, between a square and [`CARD_W_CAP`]. A still outside that range is fitted
/// (letterboxed) inside the card by [`fit_rect`], never cropped.
pub fn card_size(tw: f32, th: f32) -> (f32, f32) {
    if tw <= 0.0 || th <= 0.0 {
        return (CARD_H * 16.0 / 9.0, CARD_H);
    }
    ((CARD_H * tw / th).clamp(CARD_W_FLOOR, CARD_W_CAP), CARD_H)
}

/// Where the card of width `w` sits: centred over the knob at `knob_x`, clamped to the scrubber's
/// own extent, its bottom `CARD_GAP` above the scrubber's top at `bar_y`.
pub fn card_rect(knob_x: f32, bar_x: f32, bar_w: f32, bar_y: f32, w: f32) -> Rect {
    let x = (knob_x - w * 0.5).clamp(bar_x, (bar_x + bar_w - w).max(bar_x));
    Rect::new(x, bar_y - CARD_GAP - CARD_H, w, CARD_H)
}

/// The largest `tw`×`th`-aspect rect that fits inside `card`, centred (fit, never cover). Equal to
/// `card` itself, to within half a pixel, for every still the card's width follows.
pub fn fit_rect(card: Rect, tw: f32, th: f32) -> Rect {
    if tw <= 0.0 || th <= 0.0 {
        return card;
    }
    let k = (card.w / tw).min(card.h / th);
    let (w, h) = (tw * k, th * k);
    if (card.w - w).abs() < 0.5 && (card.h - h).abs() < 0.5 {
        return card;
    }
    Rect::new(card.x + (card.w - w) * 0.5, card.y + (card.h - h) * 0.5, w, h)
}

/// `r` scaled by `s` about the point `(ox, oy)` — the entrance grows the card from the knob.
pub fn scaled_about(r: Rect, ox: f32, oy: f32, s: f32) -> Rect {
    Rect::new(ox + (r.x - ox) * s, oy + (r.y - oy) * s, r.w * s, r.h * s)
}

/// Draw the card chrome (shadow) and the still(s) into `r`, at the painter's cascaded alpha. `tex`
/// is the render cache's texture, `(tw, th)` its decoded size; `prev` is the still being
/// crossfaded away from, as `(texture, fraction of the new one now laid over it)`.
pub fn draw(p: Painter, r: Rect, tex: u32, tw: f32, th: f32, prev: Option<(u32, f32)>) {
    p.shadow(r, CARD_RAD, CARD_SHADOW_BLUR, CARD_SHADOW_DY, theme::scrim_black(CARD_SHADOW_A));
    let inner = fit_rect(r, tw, th);
    let boxed = inner != r;
    if boxed {
        // the letterbox bars: the card's own ground
        p.rrect(r, CARD_RAD, CARD_RAD, theme::scrim_black(1.0));
    }
    let rad = if boxed { 0.0 } else { CARD_RAD };
    if let Some((old, t)) = prev {
        p.tex(old, inner, rad, theme::TINT_WHITE);
        p.alpha(t).tex(tex, inner, rad, theme::TINT_WHITE);
    } else {
        p.tex(tex, inner, rad, theme::TINT_WHITE);
    }
    p.rring(r, CARD_RAD, CARD_RIM_W, theme::CARD_SHEEN);
}

// ---- motion ---------------------------------------------------------------------------------
//
// Everything below is PURE: a value of time in, a number out, so the choreography is tested
// without a GL context. `HudMotion::step` feeds [`Motion::step_live`] once per frame (the
// scrub's edges, the chrome and the knob clock); the draw feeds [`Motion::observe`] (the card).

/// The chrome (title block, control row, tabs) fades out over this when a scrub starts…
const CHROME_OUT_MS: u32 = 150;
/// …waits this long after the scrub ends…
const CHROME_WAIT_MS: u32 = 150;
/// …and returns over this.
const CHROME_IN_MS: u32 = 250;
/// The card's entrance (first show of a scrub): fade and scale, from the knob.
const ENTRANCE_MS: u32 = 150;
/// The card's scale at the start of its entrance.
const ENTRANCE_FROM: f32 = 0.94;
/// The card hiding and re-showing inside one scrub: a plain fade.
const RESHOW_MS: u32 = 100;
/// The card leaving at the scrub's end.
const EXIT_MS: u32 = 120;
/// One still replacing the next, in place.
const SWAP_MS: u32 = 80;
/// How long the last still stays up once the target has SETTLED out of tolerance of it, counted
/// from the target's last move (or from the still going out of tolerance, whichever is later), with
/// no nearer still having arrived. Not instant: hiding at once would flicker the card.
///
/// While the target is MOVING there is no clock: a held key slews it up to 480 s/s while a still
/// takes ~80 ms to land, so the newest still ARRIVED is nearly always further than
/// [`STILL_TOLERANCE_MS`] from it, and hiding on distance would blink the card out of every fast
/// hold with stills landing all the while. Every move restarts this timer instead (see
/// [`Motion::observe`]).
pub const STALE_HIDE_MS: u32 = 400;
/// A still stands for a SETTLED target while it is within this many ms of it. The server's index
/// interval is not known to us (a PMS default is 10 s), so it is the floor and the constant.
pub const STILL_TOLERANCE_MS: i64 = 10_000;
/// The knob clock's size at full growth: the type-ladder rung nearest 1.25x its resting
/// [`theme::size::CAPTION`] (30 px). 28 and 32 are both 2 px away; [`theme::size::BODY`] is taken
/// because its bold face is already open for the control row's labels, so the first grown clock
/// opens no font of its own. A size is never animated (`gfx/tokens.rs`): the clock CROSSFADES
/// between the resting size and this one, see [`clock_faces`].
pub const CLOCK_GROWN_SZ: i32 = theme::size::BODY;
const CLOCK_UP_MS: u32 = 150;
const CLOCK_DOWN_MS: u32 = 200;
/// A scrub that has shown no card by now, and has not been told there is no index, is judged to
/// have none to show: the clock then grows. Short enough to feel at once, long enough that a
/// still arriving at the ordinary speed never sees the clock move.
const NO_CARD_GRACE_MS: u32 = 400;

/// The knob clock's faces at growth progress `grow` (0..1): `(size, opacity)` for each one drawn.
/// The resting `base` size fades out as [`CLOCK_GROWN_SZ`] fades in, both laid out at their own
/// size on one baseline; at either end one face stands alone at full opacity. Two rungs
/// crossfaded rather than a size tweened, so no frame asks the glyph cache for a size that is not
/// on the ladder.
pub fn clock_faces(base: i32, grow: f32) -> [Option<(i32, f32)>; 2] {
    let g = grow.clamp(0.0, 1.0);
    [(g < 1.0).then_some((base, 1.0 - g)), (g > 0.0).then_some((CLOCK_GROWN_SZ, g))]
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ease {
    Out,
    In,
}

/// A value travelling from `from` to `to` over `dur` ms from `start`, eased. `start` may lie in
/// the future (a delayed ramp holds `from` until then).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Ramp {
    from: f32,
    to: f32,
    start: u32,
    dur: u32,
    ease: Ease,
}

impl Ramp {
    pub const fn rest(v: f32) -> Self {
        Self { from: v, to: v, start: 0, dur: 0, ease: Ease::Out }
    }
    pub const fn new(from: f32, to: f32, start: u32, dur: u32, ease: Ease) -> Self {
        Self { from, to, start, dur, ease }
    }
    fn dt(&self, now: u32) -> i32 {
        now.wrapping_sub(self.start) as i32
    }
    pub fn at(&self, now: u32) -> f32 {
        let dt = self.dt(now);
        if dt <= 0 {
            return self.from;
        }
        if dt >= self.dur as i32 {
            return self.to;
        }
        let t = dt as f32 / self.dur as f32;
        let e = match self.ease {
            Ease::Out => 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t),
            Ease::In => t * t * t,
        };
        self.from + (self.to - self.from) * e
    }
    /// Not yet at its end (a delayed ramp is busy while it waits).
    pub fn busy(&self, now: u32) -> bool {
        self.from != self.to && self.dt(now) < self.dur as i32
    }
}

/// Is a still `shown_ms` close enough to the target to stand for it?
pub fn within_tolerance(shown_ms: i64, target_ms: i64) -> bool {
    (shown_ms - target_ms).abs() <= STILL_TOLERANCE_MS
}

/// Chrome that is this faded takes no pointer: hidden elements are not hover or click targets.
pub fn chrome_takes_pointer(alpha: f32) -> bool {
    alpha >= 0.5
}

/// What to draw for the card this frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Show {
    /// the still (ms) to draw
    pub ms: i64,
    pub alpha: f32,
    /// 0.94..1: the entrance's scale about the knob
    pub scale: f32,
    /// the knob's x the card is anchored to (the last live one, for an exit)
    pub knob_x: f32,
    /// the still being crossfaded away from, and how much of the new one is laid over it
    pub prev: Option<(i64, f32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Motion {
    live: bool,
    live_since: u32,
    chrome: Ramp,
    /// the chrome's alpha as of the last [`Motion::step_live`] — what the pointer map reads
    chrome_seen: f32,
    card: Ramp,
    pop: Ramp,
    /// a card has shown in this scrub (the entrance has played)
    entered: bool,
    /// the card is currently wanted
    card_on: bool,
    /// since when the shown still has been out of tolerance of a target that has not moved on: the
    /// target's every move restarts it, so it only runs out once the target has settled
    stale_since: Option<u32>,
    /// the target the last [`Motion::observe`] was asked about, to see it move
    last_target: Option<i64>,
    /// the memo found no index for this part
    no_index: bool,
    clock: Ramp,
    clock_up: bool,
    /// the still last drawn and where, for the exit fade
    last: Option<(i64, f32)>,
    /// the still shown before the current one, and when the current one took over
    swap: Option<(i64, u32)>,
    last_shown: Option<i64>,
}

impl Default for Motion {
    fn default() -> Self {
        Self::new()
    }
}

impl Motion {
    pub const fn new() -> Self {
        Self {
            live: false,
            live_since: 0,
            chrome: Ramp::rest(1.0),
            chrome_seen: 1.0,
            card: Ramp::rest(0.0),
            pop: Ramp::rest(1.0),
            entered: false,
            card_on: false,
            stale_since: None,
            last_target: None,
            no_index: false,
            clock: Ramp::rest(0.0),
            clock_up: false,
            last: None,
            swap: None,
            last_shown: None,
        }
    }

    /// Once per frame: the scrub's edges, the chrome, the knob clock. `live` is
    /// `TX.scrub_ns >= 0`.
    pub fn step_live(&mut self, live: bool, now: u32) {
        if live && !self.live {
            self.live = true;
            self.live_since = now;
            self.chrome = Ramp::new(self.chrome.at(now), 0.0, now, CHROME_OUT_MS, Ease::Out);
            // a new scrub opens on none of the last one's card
            self.card = Ramp::rest(0.0);
            self.pop = Ramp::rest(1.0);
            self.entered = false;
            self.card_on = false;
            self.stale_since = None;
            self.last_target = None;
            self.last = None;
            self.swap = None;
            self.last_shown = None;
        } else if !live && self.live {
            self.live = false;
            self.chrome = Ramp::new(
                self.chrome.at(now),
                1.0,
                now.wrapping_add(CHROME_WAIT_MS),
                CHROME_IN_MS,
                Ease::Out,
            );
            if self.card_on {
                self.card = Ramp::new(self.card.at(now), 0.0, now, EXIT_MS, Ease::In);
                self.card_on = false;
            }
        }
        // the knob clock grows only when this scrub has shown no card, and the answer is settled
        let up = self.live
            && !self.entered
            && (self.no_index || now.wrapping_sub(self.live_since) >= NO_CARD_GRACE_MS);
        if up != self.clock_up {
            self.clock_up = up;
            let (to, ms) = if up { (1.0, CLOCK_UP_MS) } else { (0.0, CLOCK_DOWN_MS) };
            self.clock = Ramp::new(self.clock.at(now), to, now, ms, Ease::Out);
        }
        self.chrome_seen = self.chrome.at(now);
    }

    /// The title block, control row and tabs' opacity.
    pub fn chrome_alpha(&self, now: u32) -> f32 {
        self.chrome.at(now)
    }

    /// Does the chrome take the pointer? As of the last [`Motion::step_live`]; false for the whole
    /// of a live scrub, and until the chrome is back.
    pub fn chrome_interactive(&self) -> bool {
        !self.live && chrome_takes_pointer(self.chrome_seen)
    }

    /// How far the knob clock has grown at `now`, 0 (resting) to 1 (grown): the progress its two
    /// faces crossfade on ([`clock_faces`]).
    pub fn clock_grow(&self, now: u32) -> f32 {
        self.clock.at(now).clamp(0.0, 1.0)
    }

    /// Does anything here still move at `now`? The frame loop presents on change only, so an
    /// animation owes a frame for as long as it runs.
    pub fn animating(&self, now: u32) -> bool {
        self.chrome.busy(now)
            || self.card.busy(now)
            || self.pop.busy(now)
            || self.clock.busy(now)
            || self.swap.is_some_and(|(_, at)| now.wrapping_sub(at) < SWAP_MS)
            // the grace and hide timers
            || (self.live && !self.entered && !self.no_index && now.wrapping_sub(self.live_since) < NO_CARD_GRACE_MS + 16)
            || self.stale_since.is_some_and(|t| now.wrapping_sub(t) < STALE_HIDE_MS + 16)
    }

    /// Per draw, while a scrub is live: `shown` is the memo's answer, `target_ms` the still the
    /// knob asks for. Decides whether the card is wanted and answers what to draw.
    pub fn observe(
        &mut self,
        shown: Option<i64>,
        target_ms: i64,
        absent: bool,
        knob_x: f32,
        now: u32,
    ) -> Option<Show> {
        self.no_index = absent;
        let within = shown.is_some_and(|ms| within_tolerance(ms, target_ms));
        // A target that is still MOVING (a hold, a key step, a drag) keeps the last arrived still up
        // whatever its distance: the clock restarts on every move and only runs once the target
        // has settled.
        let moved = self.last_target.replace(target_ms) != Some(target_ms);
        if shown.is_some() && !within {
            if moved {
                self.stale_since = Some(now);
            } else {
                self.stale_since.get_or_insert(now);
            }
        } else {
            self.stale_since = None;
        }
        let wanted = shown.is_some()
            && (within || self.stale_since.is_some_and(|t| now.wrapping_sub(t) < STALE_HIDE_MS));
        if wanted && !self.card_on {
            self.card_on = true;
            if self.entered {
                // back within the same scrub: a plain fade
                self.card = Ramp::new(self.card.at(now), 1.0, now, RESHOW_MS, Ease::Out);
            } else {
                self.entered = true;
                self.card = Ramp::new(0.0, 1.0, now, ENTRANCE_MS, Ease::Out);
                self.pop = Ramp::new(ENTRANCE_FROM, 1.0, now, ENTRANCE_MS, Ease::Out);
            }
        } else if !wanted && self.card_on {
            self.card_on = false;
            self.card = Ramp::new(self.card.at(now), 0.0, now, RESHOW_MS, Ease::Out);
        }
        // one still taking over from another, in place
        if let (Some(prev), Some(cur)) = (self.last_shown, shown) {
            if prev != cur && self.card_on {
                self.swap = Some((prev, now));
            }
        }
        self.last_shown = shown;
        let ms = shown?;
        let alpha = self.card.at(now);
        if alpha <= 0.0 {
            return None;
        }
        self.last = Some((ms, knob_x));
        let prev = self.swap.and_then(|(old, at)| {
            let dt = now.wrapping_sub(at);
            (dt < SWAP_MS).then(|| (old, dt as f32 / SWAP_MS as f32))
        });
        Some(Show { ms, alpha, scale: self.pop.at(now), knob_x, prev })
    }

    /// The card fading out after a scrub ended: the last still, where it was, until it is gone.
    pub fn exit(&self, now: u32) -> Option<Show> {
        if self.live || self.card_on {
            return None;
        }
        let alpha = self.card.at(now);
        let (ms, knob_x) = self.last?;
        (alpha > 0.0).then_some(Show { ms, alpha, scale: 1.0, knob_x, prev: None })
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    const GEN: &str = "g1";

    /// A scripted server: which stills exist, and how many frames each takes to arrive.
    struct Server {
        has_index: bool,
        delay: u32,
        asked: Vec<(i64, u32)>,
        frame: u32,
    }
    impl Server {
        fn new(has_index: bool, delay: u32) -> Self {
            Self { has_index, delay, asked: Vec::new(), frame: 0 }
        }
    }
    impl Stills for Server {
        fn look(&mut self, ms: i64) -> Look {
            let at = match self.asked.iter().find(|(m, _)| *m == ms) {
                Some((_, at)) => *at,
                None => {
                    self.asked.push((ms, self.frame));
                    self.frame
                }
            };
            if self.frame < at + self.delay {
                Look::Waiting
            } else if self.has_index {
                Look::Ready
            } else {
                Look::Failed
            }
        }

        fn tex(&mut self, ms: i64) -> Option<(u32, f32, f32)> {
            // one texture per still, named by its second; every still is a 16:9 tile
            self.asked.iter().any(|(m, _)| *m == ms).then_some((1 + (ms / 1000) as u32, 320.0, 180.0))
        }
    }
    fn run(m: &mut ScrubPreview, s: &mut Server, target: i64) -> Option<i64> {
        let now = s.frame * 16;
        let r = m.step((7459, GEN), target, now, |ms| s.look(ms));
        s.frame += 1;
        r
    }

    #[test]
    fn a_still_is_asked_for_at_the_knob_to_the_nearest_second() {
        // Seen on the set: flooring to the 10 s scrub step showed 12:40 for a knob at 12:44 — the
        // server already answers with its NEAREST still, so the request carries the position.
        assert_eq!(still_ms(764_000_000_000), 764_000);
        assert_eq!(still_ms(764_499_000_000), 764_000);
        assert_eq!(still_ms(764_500_000_000), 765_000);
        assert_eq!(still_ms(0), 0);
        assert_eq!(still_ms(-5), 0, "a negative preview clamps to the start");
        assert_eq!(still_path(7459, 600_000), "/library/parts/7459/indexes/sd/600000");
    }

    /// A source that cuts one still at a time out of a larger picture holds only the newest: the
    /// memo and the card read it through [`Stills`] like any other.
    struct OneTile {
        tile: Option<i64>,
    }
    impl Stills for OneTile {
        fn look(&mut self, ms: i64) -> Look {
            self.tile = Some(ms);
            Look::Ready
        }
        fn tex(&mut self, ms: i64) -> Option<(u32, f32, f32)> {
            (self.tile == Some(ms)).then_some((9, 320.0, 134.0))
        }
    }

    #[test]
    fn a_source_holding_one_tile_draws_through_the_same_memo_and_card() {
        let (mut m, mut src) = (ScrubPreview::new(), OneTile { tile: None });
        assert_eq!(m.step((7459, GEN), 30_000, 0, |ms| src.look(ms)), Some(30_000));
        assert_eq!(m.step((7459, GEN), 31_000, 16, |ms| src.look(ms)), Some(31_000));
        // the previous tile is gone: the draw's crossfade finds nothing and swaps at once
        assert_eq!(src.tex(30_000), None);
        let (tex, w, h) = src.tex(31_000).unwrap();
        assert_eq!(tex, 9);
        // a 2.39:1 tile sizes the card by its own aspect, as a poster-sourced still does
        assert_eq!(card_size(w, h), (CARD_H * 320.0 / 134.0, CARD_H));
    }

    #[test]
    fn a_still_shows_once_it_arrives_and_nothing_before() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(true, 2));
        assert_eq!(run(&mut m, &mut s, 600_000), None);
        assert_eq!(run(&mut m, &mut s, 600_000), None);
        assert_eq!(run(&mut m, &mut s, 600_000), Some(600_000));
        assert_eq!(s.asked.len(), 1, "one still, asked once");
    }

    #[test]
    fn a_part_without_an_index_is_asked_once_per_playback_and_draws_nothing() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(false, 1));
        for i in 0..20 {
            assert_eq!(run(&mut m, &mut s, i * 10_000), None);
        }
        assert_eq!(s.asked.len(), 1, "the first settled miss stops the asking");
        // the next playback of the same part asks again — the server may have generated it since
        let r = m.step((7459, "g2"), 500_000, 0, |ms| s.look(ms));
        assert_eq!(r, None);
        assert_eq!(s.asked.len(), 2);
    }

    #[test]
    fn a_held_scrub_keeps_one_request_in_flight_and_the_last_still_up() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(true, 3));
        let mut shown = Vec::new();
        // the knob moves one step every frame — faster than the server answers
        for i in 0..30 {
            if let Some(ms) = run(&mut m, &mut s, i * 10_000) {
                shown.push(ms);
            }
        }
        assert!(s.asked.len() <= 30 / 3 + 1, "asked {} stills for 30 frames", s.asked.len());
        assert!(!shown.is_empty());
        assert!(shown.windows(2).all(|w| w[0] <= w[1]), "the preview never runs backwards");
        // once the knob rests, the preview catches up to it
        for _ in 0..10 {
            run(&mut m, &mut s, 290_000);
        }
        assert_eq!(run(&mut m, &mut s, 290_000), Some(290_000));
    }

    #[test]
    fn a_miss_after_a_still_has_shown_keeps_the_last_one() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(true, 0));
        assert_eq!(run(&mut m, &mut s, 10_000), Some(10_000));
        s.has_index = false; // a later offset 404s (past the end, say)
        assert_eq!(run(&mut m, &mut s, 20_000), Some(10_000));
        s.has_index = true;
        assert_eq!(run(&mut m, &mut s, 30_000), Some(30_000), "not latched absent");
    }

    /// A scrub that ended at 10:00 must not open the next one, at 40:00, on its 10:00 still.
    #[test]
    fn a_new_scrub_never_opens_on_the_last_scrubs_still() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(true, 2));
        for _ in 0..4 {
            run(&mut m, &mut s, 600_000);
        }
        assert_eq!(run(&mut m, &mut s, 600_000), Some(600_000));
        m.release();
        assert_eq!(run(&mut m, &mut s, 2_400_000), None, "nothing until 40:00's own still lands");
    }

    /// Ending a scrub forgets the stills, not what is known about the part.
    #[test]
    fn an_absent_index_outlives_the_scrub_that_found_it() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(false, 0));
        run(&mut m, &mut s, 0);
        m.release();
        run(&mut m, &mut s, 600_000);
        assert_eq!(s.asked.len(), 1);
    }

    #[test]
    fn a_request_that_never_settles_is_given_up() {
        let mut m = ScrubPreview::new();
        let asked = std::cell::RefCell::new(Vec::new());
        let look = |ms: i64| {
            asked.borrow_mut().push(ms);
            Look::Waiting
        };
        m.step((7459, GEN), 10_000, 0, look);
        m.step((7459, GEN), 20_000, 100, look);
        assert!(!asked.borrow().contains(&20_000), "nothing new while one is in flight");
        m.step((7459, GEN), 20_000, PENDING_MAX_MS + 1, look);
        assert!(asked.borrow().contains(&20_000), "a stuck request stops blocking the next");
    }

    #[test]
    fn no_part_no_preview() {
        let mut m = ScrubPreview::new();
        assert_eq!(m.step((0, GEN), 0, 0, |_| Look::Ready), None);
    }

    #[test]
    fn the_card_is_216_tall_and_as_wide_as_the_still_up_to_518() {
        assert_eq!(card_size(320.0, 180.0), (384.0, 216.0), "16:9");
        assert_eq!(card_size(320.0, 240.0), (288.0, 216.0), "4:3");
        let (w, h) = card_size(480.0, 200.0);
        assert_eq!((w, h), (CARD_W_CAP, 216.0), "2.40:1 caps at 518");
        assert_eq!(card_size(640.0, 232.0).0, CARD_W_CAP, "wider than the cap stays at it");
        assert_eq!(card_size(180.0, 320.0).0, 216.0, "a portrait still does not make a sliver");
        assert_eq!(card_size(0.0, 0.0), (384.0, 216.0), "no size known: 16:9");
    }

    #[test]
    fn a_still_is_fitted_inside_the_card_never_cropped() {
        let card = |w| Rect::new(100.0, 400.0, w, 216.0);
        // the aspects the width follows fill the card exactly
        assert_eq!(fit_rect(card(384.0), 320.0, 180.0), card(384.0));
        assert_eq!(fit_rect(card(288.0), 320.0, 240.0), card(288.0));
        assert_eq!(fit_rect(card(518.0), 480.0, 200.0), card(518.0), "2.40:1 is within half a pixel");
        // an anamorphic 2.76:1 is letterboxed: full width, centred, shorter
        let r = fit_rect(card(518.0), 2.76, 1.0);
        assert_eq!((r.x, r.w), (100.0, 518.0));
        assert!(r.h < 216.0 && (r.y - 400.0 - (216.0 - r.h) * 0.5).abs() < 1e-3);
        // a portrait still is pillarboxed inside the square floor
        let r = fit_rect(card(216.0), 180.0, 320.0);
        assert_eq!((r.y, r.h), (400.0, 216.0));
        assert!(r.w < 216.0);
    }

    #[test]
    fn the_card_rides_the_knob_inside_the_bar() {
        let (bx, bw, by) = (96.0, 1728.0, 882.0);
        for w in [384.0, 288.0, CARD_W_CAP] {
            let mid = card_rect(960.0, bx, bw, by, w);
            assert_eq!(mid.x, 960.0 - w * 0.5);
            assert_eq!(mid.y + mid.h, by - CARD_GAP, "bottom 26 px above the bar");
            assert_eq!(mid.h, CARD_H);
            assert_eq!(card_rect(100.0, bx, bw, by, w).x, bx, "clamped at the left end");
            assert_eq!(card_rect(1820.0, bx, bw, by, w).x, bx + bw - w, "clamped at the right end");
        }
    }

    #[test]
    fn the_entrance_scales_about_the_knob() {
        let r = Rect::new(800.0, 440.0, 384.0, 216.0);
        assert_eq!(scaled_about(r, 992.0, 890.0, 1.0), r);
        let s = scaled_about(r, 992.0, 890.0, 0.94);
        assert!((s.w - 384.0 * 0.94).abs() < 1e-3 && (s.h - 216.0 * 0.94).abs() < 1e-3);
        assert!((s.cx() - 992.0).abs() < 1e-3, "still centred over the knob");
    }

    // ---- motion ----------------------------------------------------------------------------

    #[test]
    fn the_ramps_ease_and_land_exactly() {
        let out = Ramp::new(1.0, 0.0, 100, 150, Ease::Out);
        assert_eq!(out.at(50), 1.0, "before it starts");
        assert_eq!(out.at(100), 1.0);
        assert_eq!(out.at(250), 0.0);
        assert!(out.at(137) < 0.5, "ease-out is front-loaded: {}", out.at(137));
        let inn = Ramp::new(1.0, 0.0, 0, 120, Ease::In);
        assert!(inn.at(60) > 0.5, "ease-in is back-loaded");
        assert!(out.busy(200) && !out.busy(250) && !Ramp::rest(1.0).busy(0));
    }

    #[test]
    fn a_scrub_fades_the_chrome_out_in_150_and_back_after_150_over_250() {
        let mut m = Motion::new();
        m.step_live(false, 1000);
        assert_eq!(m.chrome_alpha(1000), 1.0);
        assert!(m.chrome_interactive());
        m.step_live(true, 1000);
        assert!(m.chrome_alpha(1075) < 1.0 && m.chrome_alpha(1075) > 0.0);
        assert_eq!(m.chrome_alpha(1150), 0.0);
        m.step_live(true, 5000);
        assert_eq!(m.chrome_alpha(5000), 0.0, "stays down for the whole scrub");
        m.step_live(false, 6000);
        assert_eq!(m.chrome_alpha(6149), 0.0, "150 ms wait");
        assert!(m.chrome_alpha(6275) > 0.0 && m.chrome_alpha(6275) < 1.0);
        assert_eq!(m.chrome_alpha(6400), 1.0, "250 ms after the wait");
    }

    /// The chrome is not a hover or click target while hidden: not during the scrub, not in the
    /// wait, and not until it is back over half.
    #[test]
    fn hidden_chrome_takes_no_pointer() {
        assert!(!chrome_takes_pointer(0.0) && !chrome_takes_pointer(0.49));
        assert!(chrome_takes_pointer(0.5) && chrome_takes_pointer(1.0));
        let mut m = Motion::new();
        m.step_live(true, 0);
        assert!(!m.chrome_interactive(), "the instant the scrub starts");
        m.step_live(true, 100);
        assert!(!m.chrome_interactive());
        m.step_live(false, 2000);
        for t in [2000, 2100, 2160, 2200] {
            m.step_live(false, t);
            assert!(!m.chrome_interactive(), "{t}: still hidden or under half");
        }
        m.step_live(false, 2300);
        assert!(m.chrome_interactive(), "back over half");
    }

    #[test]
    fn a_scrub_that_restarts_mid_restore_fades_from_where_it_was() {
        let mut m = Motion::new();
        m.step_live(true, 0);
        m.step_live(false, 1000);
        let mid = m.chrome_alpha(1275);
        assert!(mid > 0.0 && mid < 1.0);
        m.step_live(true, 1275);
        assert_eq!(m.chrome_alpha(1275), mid, "no jump");
        assert_eq!(m.chrome_alpha(1425), 0.0);
    }

    fn live(shown: Option<i64>, target: i64, now: u32, m: &mut Motion) -> Option<Show> {
        m.step_live(true, now);
        m.observe(shown, target, false, 900.0, now)
    }

    #[test]
    fn a_still_within_ten_seconds_stands_for_the_target() {
        assert!(within_tolerance(600_000, 600_000));
        assert!(within_tolerance(600_000, 610_000));
        assert!(within_tolerance(610_000, 600_000));
        assert!(!within_tolerance(600_000, 610_001));
        assert!(!within_tolerance(600_000, 589_999));
    }

    #[test]
    fn a_stale_card_hides_after_400_ms_not_at_once() {
        let mut m = Motion::new();
        assert!(live(Some(600_000), 600_000, 0, &mut m).is_some() || m.card_on);
        assert!(live(Some(600_000), 600_000, 200, &mut m).is_some());
        // the knob runs 100 s away and settles there; no nearer still arrives
        assert!(live(Some(600_000), 700_000, 300, &mut m).is_some(), "still up at once");
        assert!(live(Some(600_000), 700_000, 600, &mut m).is_some(), "still up at 300 ms stale");
        live(Some(600_000), 700_000, 700, &mut m);
        assert!(m.stale_since.is_some() && !m.card_on, "at 400 ms stale the card is asked to hide");
        let fading = live(Some(600_000), 700_000, 750, &mut m).unwrap();
        assert!(fading.alpha < 1.0 && fading.alpha > 0.0, "a 100 ms fade, not a cut: {}", fading.alpha);
        assert!(live(Some(600_000), 700_000, 900, &mut m).is_none(), "gone after its 100 ms fade");
    }

    /// **A fast hold keeps its card.** A held key at `SCRUB_MAX` (480 s/s) moves the target ~7.7 s
    /// per 16 ms frame, and a still takes ~80 ms to land, so the newest still ARRIVED is always far
    /// outside the 10 s tolerance of a target that has run on. Measured on the set: 10-14 stills a
    /// second all answered, and the card still blinked out 400 ms into every hold. While the target
    /// moves the last arrived still stands for it, whatever its distance.
    #[test]
    fn a_fast_hold_keeps_the_card_while_stills_keep_arriving() {
        let (mut m, mut s) = (ScrubPreview::new(), Server::new(true, 5));
        let mut hidden = Vec::new();
        for i in 0..200u32 {
            let now = i * 16;
            let target = still_ms(60_000_000_000 + i64::from(i) * 7_680_000_000);
            m.motion.step_live(true, now);
            let shown = m.step((7459, GEN), target, now, |ms| s.look(ms));
            s.frame += 1;
            let card = m.motion.observe(shown, target, false, 900.0, now);
            if now > 1_000 && card.is_none_or(|c| c.alpha < 1.0) {
                hidden.push(now);
            }
        }
        assert!(hidden.is_empty(), "the card dropped out at {hidden:?} ms of a hold with stills landing");
    }

    /// …and the tolerance is for a target that has SETTLED: a knob that stops with only a far still
    /// shown hides it after the 400 ms, counted from the last move.
    #[test]
    fn a_settled_target_with_only_a_far_still_hides_it_400_ms_after_the_last_move() {
        let mut m = Motion::new();
        for (i, t) in (0..=1000).step_by(100).enumerate() {
            let card = live(Some(600_000), 600_000 + 60_000 * (i as i64 + 1), t, &mut m);
            assert!(t == 0 || card.is_some(), "moving at {t}"); // t = 0 is the entrance's first frame
        }
        // the target stops at 1_260_000 (its last move was at 1000 ms); the shown still is 11 minutes behind
        let at = |t: u32, m: &mut Motion| live(Some(600_000), 1_260_000, t, m);
        assert!(at(1_100, &mut m).is_some() && at(1_390, &mut m).is_some(), "up through the 400 ms after the last move");
        assert!(m.card_on);
        at(1_400, &mut m);
        assert!(!m.card_on, "hidden 400 ms after the target settled: one clock, not a settle window plus a grace");
    }

    #[test]
    fn a_nearer_still_inside_the_window_keeps_the_card() {
        let mut m = Motion::new();
        live(Some(600_000), 600_000, 0, &mut m);
        live(Some(600_000), 660_000, 100, &mut m);
        assert!(live(Some(660_000), 660_000, 300, &mut m).is_some());
        assert!(m.stale_since.is_none(), "the timer resets");
        assert!(live(Some(660_000), 660_000, 1000, &mut m).is_some());
    }

    #[test]
    fn the_entrance_plays_once_per_scrub_and_a_reshow_is_a_plain_fade() {
        let mut m = Motion::new();
        let a = live(Some(600_000), 600_000, 0, &mut m);
        assert!(a.is_none() || a.unwrap().scale < 1.0);
        let mid = live(Some(600_000), 600_000, 75, &mut m).unwrap();
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0 && mid.scale > ENTRANCE_FROM && mid.scale < 1.0);
        let full = live(Some(600_000), 600_000, 200, &mut m).unwrap();
        assert_eq!((full.alpha, full.scale), (1.0, 1.0));
        // out of tolerance for good, then a still arrives again
        for t in (300..1000).step_by(100) {
            live(Some(600_000), 800_000, t, &mut m);
        }
        assert!(live(Some(600_000), 800_000, 1100, &mut m).is_none());
        let back = live(Some(800_000), 800_000, 1200, &mut m);
        assert!(back.map_or(true, |s| s.scale == 1.0), "no scale on a reshow");
        let mid = live(Some(800_000), 800_000, 1250, &mut m).unwrap();
        assert_eq!(mid.scale, 1.0, "no scale on a reshow");
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0, "a 100 ms fade: {}", mid.alpha);
        assert_eq!(live(Some(800_000), 800_000, 1300, &mut m).unwrap().alpha, 1.0);
        // a NEW scrub plays the entrance again
        m.step_live(false, 3000);
        m.step_live(true, 5000);
        m.observe(Some(10_000), 10_000, false, 900.0, 5000);
        let s = m.observe(Some(10_000), 10_000, false, 900.0, 5075).unwrap();
        assert!(s.scale < 1.0);
    }

    #[test]
    fn the_card_fades_out_over_120_ms_when_the_scrub_ends_and_a_new_one_drops_it() {
        let mut m = Motion::new();
        live(Some(600_000), 600_000, 0, &mut m);
        live(Some(600_000), 600_000, 300, &mut m);
        assert!(m.exit(300).is_none(), "not while live");
        m.step_live(false, 1000);
        let s = m.exit(1000).unwrap();
        assert_eq!((s.ms, s.knob_x, s.alpha), (600_000, 900.0, 1.0), "the last still, where it was");
        let mid = m.exit(1060).unwrap();
        assert!(mid.alpha > 0.0 && mid.alpha < 1.0);
        assert!(m.exit(1120).is_none());
        // a card that was up and a scrub that starts again does not inherit it
        let mut m = Motion::new();
        live(Some(600_000), 600_000, 0, &mut m);
        live(Some(600_000), 600_000, 300, &mut m);
        m.step_live(false, 1000);
        m.step_live(true, 1040);
        assert!(m.exit(1050).is_none());
    }

    #[test]
    fn a_swap_crossfades_in_80_ms_in_place() {
        let mut m = Motion::new();
        live(Some(600_000), 600_000, 0, &mut m);
        live(Some(600_000), 600_000, 300, &mut m);
        let s = live(Some(606_000), 606_000, 400, &mut m).unwrap();
        assert_eq!(s.prev, Some((600_000, 0.0)));
        let s = live(Some(606_000), 606_000, 440, &mut m).unwrap();
        assert_eq!(s.prev.map(|p| p.1), Some(0.5));
        assert_eq!(live(Some(606_000), 606_000, 480, &mut m).unwrap().prev, None);
    }

    /// **A size cannot be animated** (`gfx/tokens.rs`): across the whole ramp up and back down,
    /// the knob clock draws at the resting size and ONE ladder rung above it, crossfaded on the
    /// ramp's progress — never at a size in between.
    #[test]
    fn the_knob_clock_crossfades_two_ladder_rungs() {
        use plx_ui::theme::size::{BODY, CAPTION};
        assert_eq!(CLOCK_GROWN_SZ, BODY, "the rung nearest 1.25x CAPTION");
        let mut m = Motion::new();
        m.step_live(true, 0);
        m.observe(None, 0, true, 900.0, 0);
        let mut seen = std::collections::BTreeSet::new();
        let mut mid_fade = false;
        let mut walk = |m: &Motion, t: u32| {
            let faces = clock_faces(CAPTION, m.clock_grow(t));
            let drawn: Vec<(i32, f32)> = faces.into_iter().flatten().collect();
            for &(sz, a) in &drawn {
                seen.insert(sz);
                assert!(a > 0.0 && a <= 1.0, "{t}: a drawn face is visible: {drawn:?}");
            }
            if drawn.len() == 2 {
                mid_fade = true;
                assert!((drawn[0].1 + drawn[1].1 - 1.0).abs() < 1e-6, "{t}: opposite alphas: {drawn:?}");
            }
        };
        for t in (0..=600).step_by(5) {
            m.step_live(true, t);
            walk(&m, t);
        }
        m.step_live(false, 1000);
        for t in (1000..=1400).step_by(5) {
            walk(&m, t);
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![CAPTION, BODY]);
        assert!(mid_fade, "the ramps were sampled mid-fade");
    }

    /// The knob clock grows only when the whole scrub has shown no card.
    #[test]
    fn the_knob_clock_grows_only_when_no_card_shows() {
        let grow = |m: &Motion, t: u32| m.clock_grow(t);
        // no index at all: grows at once, in 150 ms
        let mut m = Motion::new();
        m.step_live(true, 0);
        m.observe(None, 0, true, 900.0, 0);
        m.step_live(true, 16);
        assert_eq!(grow(&m, 16 + CLOCK_UP_MS), 1.0);
        let mid = grow(&m, 16 + CLOCK_UP_MS / 2);
        assert!(mid > 0.0 && mid < 1.0, "eased up: {mid}");
        // ends: back down over 200 ms
        m.step_live(false, 2000);
        assert!(grow(&m, 2100) < 1.0 && grow(&m, 2100) > 0.0);
        assert_eq!(grow(&m, 2200), 0.0);

        // a still pending the whole time: grows once the grace has passed
        let mut m = Motion::new();
        m.step_live(true, 0);
        m.observe(None, 0, false, 900.0, 0);
        m.step_live(true, 300);
        assert_eq!(grow(&m, 300 + CLOCK_UP_MS), 0.0, "not before the grace");
        m.step_live(true, NO_CARD_GRACE_MS);
        assert_eq!(grow(&m, NO_CARD_GRACE_MS + CLOCK_UP_MS), 1.0);

        // a card shows quickly: the clock never moves
        let mut m = Motion::new();
        m.step_live(true, 0);
        m.observe(Some(0), 0, false, 900.0, 100);
        for t in (100..1500).step_by(50) {
            m.step_live(true, t);
            assert_eq!(grow(&m, t + 400), 0.0, "{t}");
        }

        // a card that shows AFTER the clock grew takes it back down: never both
        let mut m = Motion::new();
        m.step_live(true, 0);
        m.step_live(true, 500);
        assert_eq!(grow(&m, 500 + CLOCK_UP_MS), 1.0);
        m.observe(Some(0), 0, false, 900.0, 700);
        m.step_live(true, 700);
        assert_eq!(grow(&m, 700 + CLOCK_DOWN_MS), 0.0);
    }

    #[test]
    fn a_resting_or_fully_grown_clock_is_one_face() {
        use plx_ui::theme::size::CAPTION;
        assert_eq!(clock_faces(CAPTION, 0.0), [Some((CAPTION, 1.0)), None]);
        assert_eq!(clock_faces(CAPTION, 1.0), [None, Some((CLOCK_GROWN_SZ, 1.0))]);
    }

    #[test]
    fn the_scrubs_motion_owes_frames_only_while_it_runs() {
        let mut m = Motion::new();
        assert!(!m.animating(0));
        m.step_live(true, 0);
        assert!(m.animating(10), "the chrome is fading");
        m.observe(None, 0, true, 900.0, 200);
        m.step_live(true, 200);
        assert!(m.animating(210), "the clock is growing");
        assert!(!m.animating(200 + CLOCK_UP_MS + 20), "all settled: the media clock carries it");
    }
}
