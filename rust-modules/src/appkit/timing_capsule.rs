//! **The Subtitle Timing capsule**: the on-video "‹ Subtitles 0.3 s later ›" control the Subtitles
//! panel's Timing row hands off to (`appkit::track_menu::TrackOk::OpenTiming`,
//! `screens::player::overlay::OverlayKind::Timing`, plan `subtitle-menu-capsule` §4). The design
//! mock is `/tmp/dsplayer/player.ref.html`'s capsule variant (`grep capsule/shake/capsuleText`).
//!
//! This module is pure over plain values — no `PlaybackSession`, no player statics — so every rule
//! below is host-tested without a fixture: the step size, the fast step after a held run, the zero
//! detent, the limit shake and the read-out text. [`screens::player::overlay`]'s `PlayerOverlayScreen`
//! is the only caller: it owns the key's cadence gate (its existing `RepeatGate` at
//! `PANEL_REPEAT_MS`, the same one every other panel here paces a held direction with) and asks
//! [`TimingCapsule::key`] only for an ADMITTED press — a `Down` or an already-gated `Repeat`, never
//! a raw hardware repeat this module would have to re-gate itself.
use crate::ui::consts::Key;
use crate::ui::icons::{self, Icon};
use crate::ui::label::{HAlign, Label};
use plx_machine::machine::Edge;
use crate::ui::{theme, Painter, Rect, Spring};
use std::ffi::CString;

/// The capsule's step for an ordinary tap or the first few repeats of a held press
/// ([`crate::player::SUBTITLE_OFFSET_STEP_MS`]).
const STEP_MS: i64 = crate::player::SUBTITLE_OFFSET_STEP_MS;
/// The faster step a held press earns after [`FAST_AFTER_REPEATS`] admitted repeats
/// ([`crate::player::SUBTITLE_OFFSET_FAST_STEP_MS`]).
const FAST_STEP_MS: i64 = crate::player::SUBTITLE_OFFSET_FAST_STEP_MS;
/// How many ADMITTED repeats (not raw hardware ticks — the overlay's own `RepeatGate` already
/// thinned those) a held press takes before [`FAST_STEP_MS`] replaces [`STEP_MS`].
const FAST_AFTER_REPEATS: u32 = 8;
/// How long a value that lands on (or is swept across) zero rests there before a further HELD step
/// is allowed to move it again — "the file's own timing is findable blind" (mock's `stepTiming`).
/// A fresh, non-repeat press always passes regardless.
pub(crate) const DETENT_MS: u32 = 600;

/// The underdamped spring's stiffness and damping for the limit shake — tuned to land close to the
/// mock's four beats (`1, -0.7, 0.35, 0` over ~70 ms steps) without reproducing its discrete
/// timeout chain: this app has one continuous spring mechanism (`Spring::step_zeta`,
/// `ui::press`'s click spring-back) and reuses it here rather than adding a second animation
/// style.
const SHAKE_K: f32 = 900.0;
const SHAKE_ZETA: f32 = 0.22;
/// The velocity (px/s) a Bump kicks the shake spring with — chosen so the first beat's peak
/// displacement lands near the mock's 12 px.
const SHAKE_KICK: f32 = 900.0;

/// The capsule's own height (mock: `height:84px`) — also the diameter of its `rrect`'s corner
/// radius (`CAPSULE_H / 2`, a full pill).
pub(crate) const CAPSULE_H: f32 = 84.0;
/// The fixed width of the centred text block the capsule's sentence is laid out in (mock:
/// `width` is unset and the English copy never wraps in its 520). 580 because the longest shipped
/// sentence is Spanish, "Subtítulos 30,0 s más tarde" at 532 px in the bold TITLE face;
/// `the_capsule_sentence_fits_its_block_in_every_language` measures every language with the TV's
/// own advances, so a longer translation fails there rather than clipping on the set.
pub(crate) const CAPSULE_TEXT_W: f32 = 580.0;
/// The gap between each chevron and the text block (mock: `gap:22px`).
pub(crate) const CAPSULE_GAP: f32 = 22.0;
/// Horizontal padding inside the pill, chevron to edge (mock: `padding:0 26px`).
const CAPSULE_PAD_X: f32 = 26.0;
/// The chevron glyph's own box (mock's `<svg width="18" height="30">`).
const CHEVRON_SZ: f32 = 30.0;

/// **The one offset formatter**: `ms` as seconds to the tenth in `locale`'s own decimal and
/// seconds unit (`core.seconds`). `signed` prefixes `+`/`-` (the Subtitles panel's Timing
/// read-out); unsigned is the bare magnitude this capsule's sentence says "later"/"earlier" about.
/// ASCII hyphen-minus rather than U+2212, which the UI font is not guaranteed to carry.
pub(crate) fn offset_seconds_in(ms: i64, signed: bool, locale: &crate::i18n::LocaleContext) -> String {
    let sign = match ms.signum() {
        1 if signed => "+",
        -1 if signed => "-",
        _ => "",
    };
    let tenths = (ms.unsigned_abs() / 100) as i64;
    crate::i18n::msg::core_seconds_in(locale, &format!("{sign}{}", locale.decimal(tenths, 1)))
}

/// The capsule's sentence for `offset_ms` in `locale` — catalog text, with the magnitude from
/// [`offset_seconds_in`], as the Timing row's read-out uses.
fn text_in(offset_ms: i64, locale: &crate::i18n::LocaleContext) -> String {
    use crate::i18n::msg;
    if offset_ms == 0 {
        return msg::widgets_tracks_capsule_original_in(locale).to_string();
    }
    let duration = offset_seconds_in(offset_ms, false, locale);
    if offset_ms > 0 {
        msg::widgets_tracks_capsule_later_in(locale, &duration)
    } else {
        msg::widgets_tracks_capsule_earlier_in(locale, &duration)
    }
}

/// What a key did to the capsule, for the caller to act on — [`Step`](CapsuleOut::Step) commits a
/// new offset through the ordinary `TrackCommit::SubtitleOffset` path, [`Close`](CapsuleOut::Close)
/// asks the overlay to dismiss, and [`Bump`](CapsuleOut::Bump) is a pure animation cue: the value
/// did not move, only the shake did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CapsuleOut {
    Step(i64),
    Close,
    Bump,
}

/// The capsule's whole state. `offset_ms` starts at whatever the Subtitles panel's Timing row was
/// showing (`TrackMenuState::offset_ms`), and `lo`/`hi` are seeded once, at open, from
/// [`crate::player::subtitle_offset_range_ms`] — the ONE range rule the player's own clamp
/// (`player::set_subtitle_offset`) also obeys, so this control can never offer a step the player
/// refuses.
pub(crate) struct TimingCapsule {
    offset_ms: i64,
    lo: i64,
    hi: i64,
    /// Admitted repeats since the last fresh `Down` — [`FAST_AFTER_REPEATS`]'s counter.
    repeats: u32,
    /// Set when a held step has just landed the value on zero; cleared once `now` passes it or a
    /// fresh `Down` arrives. `None` means no detent is active.
    detent_until: Option<u32>,
    /// The limit shake — `pos` is the x offset [`Self::draw`] translates the capsule by.
    shake: Spring,
    /// The sentence for `offset_ms`, formatted once per change of the value ([`Self::set_offset`])
    /// rather than on every drawn frame.
    caption: CString,
}

impl TimingCapsule {
    pub(crate) fn new(offset_ms: i64, lo: i64, hi: i64) -> Self {
        let mut c = Self {
            offset_ms: 0,
            lo,
            hi,
            repeats: 0,
            detent_until: None,
            shake: Spring::at(0.0),
            caption: CString::default(),
        };
        c.set_offset(offset_ms.clamp(lo, hi));
        c
    }

    /// The ONE writer of `offset_ms`, so the cached caption can never describe a stale value.
    fn set_offset(&mut self, ms: i64) {
        self.offset_ms = ms;
        // catalog text never carries a NUL; an empty caption is the harmless fallback
        self.caption = CString::new(text_in(ms, crate::i18n::current())).unwrap_or_default();
    }

    #[cfg(test)]
    pub(crate) fn offset_ms(&self) -> i64 {
        self.offset_ms
    }

    /// One admitted key. `edge` is `Down` for a fresh press or an already-gated `Repeat` — the
    /// caller's own `RepeatGate` has already thinned the hardware's raw repeats to
    /// `PANEL_REPEAT_MS`, so every `Repeat` this sees counts toward [`FAST_AFTER_REPEATS`].
    pub(crate) fn key(&mut self, k: Key, edge: Edge, now: u32) -> Option<CapsuleOut> {
        match k {
            Key::Left { .. } | Key::Right { .. } if edge != Edge::Up => {
                let dir: i64 = if matches!(k, Key::Left { .. }) { -1 } else { 1 };
                let repeat = edge == Edge::Repeat;
                if !repeat {
                    // a fresh press starts a new gesture: no fast-step count, and no detent
                    // carried over from the last hold to swallow this one's repeats
                    self.repeats = 0;
                    self.detent_until = None;
                }
                self.step(dir, repeat, now)
            }
            // The vertical DOWN key resets to 0 outright — no hint, no detent, no shake; it always
            // passes even inside a running detent, the same as a fresh horizontal `Down` does.
            Key::Down if edge == Edge::Down => {
                self.repeats = 0;
                self.detent_until = None;
                self.set_offset(0);
                Some(CapsuleOut::Step(0))
            }
            Key::Ok | Key::Back if edge == Edge::Down => Some(CapsuleOut::Close),
            _ => None,
        }
    }

    /// The shared step math for LEFT/RIGHT — the mock's `stepTiming`, ported. `repeat` distinguishes
    /// a fresh tap (always passes, ignores any running detent) from an admitted hold (counts toward
    /// the fast step, and is the only kind the zero detent swallows).
    fn step(&mut self, dir: i64, repeat: bool, now: u32) -> Option<CapsuleOut> {
        if repeat {
            if let Some(until) = self.detent_until {
                if now < until {
                    return None; // resting in the zero detent
                }
                self.detent_until = None;
            }
            self.repeats += 1;
        }
        let step_ms = if self.repeats >= FAST_AFTER_REPEATS { FAST_STEP_MS } else { STEP_MS };
        let mut next = self.offset_ms + dir * step_ms;
        // A HELD sweep that reaches or crosses zero stops on it: the file's own timing is
        // findable blind.
        if repeat && self.offset_ms != 0 && (next == 0 || next.signum() != self.offset_ms.signum()) {
            next = 0;
            self.detent_until = Some(now.wrapping_add(DETENT_MS));
        }
        if next < self.lo || next > self.hi {
            let clamped = next.clamp(self.lo, self.hi);
            if clamped == self.offset_ms {
                // Already there: kick the shake and say why, rather than move nothing silently.
                self.shake.vel = if dir < 0 { -SHAKE_KICK } else { SHAKE_KICK };
                return Some(CapsuleOut::Bump);
            }
            next = clamped;
        }
        self.set_offset(next);
        Some(CapsuleOut::Step(next))
    }

    /// Advance the limit shake one frame — called from the overlay's `Tick`, every frame the
    /// capsule is up, whether or not a key landed this frame.
    pub(crate) fn update(&mut self, dt: f32) {
        self.shake.step_zeta(0.0, SHAKE_K, SHAKE_ZETA, dt);
    }

    /// The capsule's sentence: what HAPPENS to the subtitles, never a signed number
    /// (`player.ref.html`'s `capsuleText`, adapted to this app's own leading word).
    #[cfg(test)]
    fn text(&self) -> &str {
        self.caption.to_str().unwrap_or_default()
    }

    /// Is the LEFT (earlier/lower) direction at its limit right now? Dims that chevron
    /// ([`theme::INK_DISABLED`]).
    fn at_lo(&self) -> bool {
        self.offset_ms <= self.lo
    }
    /// The RIGHT (later/upper) direction's twin.
    fn at_hi(&self) -> bool {
        self.offset_ms >= self.hi
    }

    /// Paint the capsule, its bottom edge fixed at `bottom_y` (`player_hud::CAPSULE_BOTTOM_Y`) —
    /// never the live caption's own baseline, which would make the capsule jump as a cue's line
    /// count changes. `appear` is the container's own appear fraction (`DrawFrame::page_alpha`),
    /// the same value every other player panel fades on.
    pub(crate) fn draw(&self, bottom_y: f32, appear: f32) {
        if appear <= 0.0 {
            return;
        }
        // the container's appear fraction fades the capsule in and out, as every player panel does
        let p = Painter::root().alpha(appear);
        let w = CHEVRON_SZ * 2.0 + CAPSULE_GAP * 2.0 + CAPSULE_TEXT_W + CAPSULE_PAD_X * 2.0;
        let cx = crate::ui::consts::SCR_W * 0.5 + self.shake.pos;
        let r = Rect::new(cx - w * 0.5, bottom_y - CAPSULE_H, w, CAPSULE_H);
        p.rect(r, CAPSULE_H * 0.5, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
        let left_ink = if self.at_lo() { theme::INK_DISABLED } else { theme::TEXT_PRIMARY };
        let right_ink = if self.at_hi() { theme::INK_DISABLED } else { theme::TEXT_PRIMARY };
        icons::draw(
            p,
            Icon::ChevronLeft,
            Rect::new(r.x + CAPSULE_PAD_X, r.y + (CAPSULE_H - CHEVRON_SZ) * 0.5, CHEVRON_SZ, CHEVRON_SZ),
            left_ink,
        );
        icons::draw(
            p,
            Icon::Chevron,
            Rect::new(r.x + r.w - CAPSULE_PAD_X - CHEVRON_SZ, r.y + (CAPSULE_H - CHEVRON_SZ) * 0.5, CHEVRON_SZ, CHEVRON_SZ),
            right_ink,
        );
        let text_x = r.x + CAPSULE_PAD_X + CHEVRON_SZ + CAPSULE_GAP;
        Label::new(self.caption.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .h(HAlign::Center)
            .draw(p, Rect::new(text_x, r.y, CAPSULE_TEXT_W, r.h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap() -> TimingCapsule {
        TimingCapsule::new(0, 0, 30_000) // embedded floor: no advance
    }

    fn sidecar_cap() -> TimingCapsule {
        TimingCapsule::new(0, -60_000, 60_000)
    }

    #[test]
    fn a_tap_steps_100() {
        let mut c = cap();
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Down, 0), Some(CapsuleOut::Step(100)));
        assert_eq!(c.offset_ms(), 100);
    }

    #[test]
    fn after_8_repeats_the_step_is_500() {
        let mut c = sidecar_cap();
        c.key(Key::Right { alt: false }, Edge::Down, 0);
        let mut now = 0u32;
        for _ in 0..7 {
            now += 110;
            c.key(Key::Right { alt: false }, Edge::Repeat, now);
        }
        assert_eq!(c.offset_ms(), 100 + 7 * 100); // still the slow step through repeat #7
        now += 110;
        let out = c.key(Key::Right { alt: false }, Edge::Repeat, now);
        assert_eq!(out, Some(CapsuleOut::Step(100 + 7 * 100 + 500))); // repeat #8: fast step
    }

    #[test]
    fn the_zero_detent_swallows_repeats_for_600ms_and_a_fresh_down_passes() {
        let mut c = sidecar_cap();
        c.key(Key::Left { alt: false }, Edge::Down, 0); // -100
        // held back toward 0 lands exactly on it: the crossing/landing detent kicks in
        let out = c.key(Key::Right { alt: false }, Edge::Repeat, 110);
        assert_eq!(out, Some(CapsuleOut::Step(0)));
        assert_eq!(c.offset_ms(), 0);
        // held repeats inside the detent are swallowed
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Repeat, 300), None);
        assert_eq!(c.offset_ms(), 0);
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Repeat, 699), None);
        // a fresh Down passes even inside the detent window
        let out = c.key(Key::Right { alt: false }, Edge::Down, 650);
        assert_eq!(out, Some(CapsuleOut::Step(100)));
    }

    #[test]
    fn the_embedded_floor_is_0_and_the_sidecar_floor_is_minus_60s() {
        assert_eq!(cap().lo, 0);
        assert_eq!(sidecar_cap().lo, -60_000);
    }

    #[test]
    fn past_the_limit_bumps_leaves_the_value_unchanged_and_the_spring_settles_to_0() {
        let mut c = TimingCapsule::new(60_000, 0, 60_000);
        let out = c.key(Key::Right { alt: false }, Edge::Down, 0);
        assert_eq!(out, Some(CapsuleOut::Bump));
        assert_eq!(c.offset_ms(), 60_000);
        assert_ne!(c.shake.vel, 0.0);
        for _ in 0..600 {
            c.update(1.0 / 60.0);
        }
        assert!(c.shake.pos.abs() < 0.01, "pos={}", c.shake.pos);
        assert!(c.shake.vel.abs() < 0.01, "vel={}", c.shake.vel);
    }

    #[test]
    fn down_resets() {
        let mut c = sidecar_cap();
        c.key(Key::Left { alt: false }, Edge::Down, 0);
        c.key(Key::Left { alt: false }, Edge::Down, 0);
        assert_eq!(c.offset_ms(), -200);
        let out = c.key(Key::Down, Edge::Down, 0);
        assert_eq!(out, Some(CapsuleOut::Step(0)));
        assert_eq!(c.offset_ms(), 0);
    }

    #[test]
    fn the_text_for_plus_zero_and_minus() {
        assert_eq!(TimingCapsule::new(0, 0, 60_000).text(), "Original timing");
        assert_eq!(TimingCapsule::new(300, 0, 60_000).text(), "Subtitles 0.3 s later");
        assert_eq!(TimingCapsule::new(-300, -60_000, 60_000).text(), "Subtitles 0.3 s earlier");
    }

    #[test]
    fn ok_and_back_close() {
        let mut c = cap();
        assert_eq!(c.key(Key::Ok, Edge::Down, 0), Some(CapsuleOut::Close));
        assert_eq!(c.key(Key::Back, Edge::Down, 0), Some(CapsuleOut::Close));
    }

    #[test]
    fn up_edge_is_ignored() {
        let mut c = cap();
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Up, 0), None);
        assert_eq!(c.offset_ms(), 0);
    }

    /// A fresh press starts a new gesture: a detent the LAST hold set must not swallow the
    /// repeats of this one.
    #[test]
    fn a_fresh_down_clears_a_running_detent() {
        let mut c = sidecar_cap();
        c.key(Key::Left { alt: false }, Edge::Down, 0); // -100
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Repeat, 110), Some(CapsuleOut::Step(0)));
        // detent runs until 710; a fresh Down at 650 steps, and ITS hold's repeat at 700 moves
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Down, 650), Some(CapsuleOut::Step(100)));
        assert_eq!(c.key(Key::Right { alt: false }, Edge::Repeat, 700), Some(CapsuleOut::Step(200)));
    }

    /// The sentence is catalog text with the locale's decimal and seconds unit.
    #[test]
    fn the_text_is_localized() {
        use crate::i18n::{LocaleContext, Preference};
        let es = LocaleContext::resolve(Preference::Es, None, Some("es-ES"), None, None);
        let be = LocaleContext::resolve(Preference::Be, None, Some("be-BY"), None, None);
        assert_eq!(text_in(300, &es), "Subtítulos 0,3 s más tarde");
        assert_eq!(text_in(-300, &be), "Субцітры на 0,3 с раней");
        assert_eq!(text_in(0, &es), "Sincronización original");
    }

    /// **The capsule's sentence fits its fixed [`CAPSULE_TEXT_W`] block in every shipped
    /// language**, at the widest magnitude the range allows (30.0 s), measured with the device's
    /// whole-pixel advances in the bold face `draw` uses.
    #[test]
    fn the_capsule_sentence_fits_its_block_in_every_language() {
        use plx_base::fontcov::advances::ShippedMeasure;
        use crate::ui::fit::HEADROOM;
        use crate::i18n::{LocaleContext, Preference};
        use plx_machine::machine::Measure;
        let mut out = Vec::new();
        for (language, region) in [(Preference::En, "en-US"), (Preference::Es, "es-ES"), (Preference::Be, "be-BY")] {
            let locale = LocaleContext::resolve(language, None, Some(region), None, None);
            for ms in [-60_000, 60_000, 0] {
                let t = text_in(ms, &locale);
                let w = ShippedMeasure.width_str(&t, theme::size::TITLE, true);
                if w > CAPSULE_TEXT_W * HEADROOM {
                    out.push(format!("{}: {t:?} is {w:.0}px in {CAPSULE_TEXT_W:.0}px", language.tag()));
                }
            }
        }
        assert!(out.is_empty(), "capsule text the television would clip:\n  {}", out.join("\n  "));
    }
}
