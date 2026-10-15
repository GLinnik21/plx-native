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
//! The memo is a RENDER memo, like [`crate::player_hud::TransportRow`]'s label widths: what it
//! holds is which still the draw can show, and it is stepped where the art is probed — in the draw.

use plx_ui::{theme, Painter, Rect};

/// The stills' request size: the BIF's own 320×180, so the server's transcoder never upscales.
pub const REQ_W: i32 = 320;
pub const REQ_H: i32 = 180;
/// The drawn card: the still at 1.2×, 16:9.
pub const CARD_W: f32 = 384.0;
pub const CARD_H: f32 = 216.0;
const CARD_RAD: f32 = 10.0;
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

/// The scrub preview's memo. Owned by the player's [`crate::player_hud::TransportRow`].
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
        Self { owner: None, absent: false, shown: None, pending: None }
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
            *self = Self { owner: Some((owner.0, owner.1.to_owned())), ..Self::new() };
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
}

/// Where the card sits: centred over the knob at `knob_x`, clamped to the scrubber's own extent,
/// its bottom `CARD_GAP` above the scrubber's top at `bar_y`.
pub fn card_rect(knob_x: f32, bar_x: f32, bar_w: f32, bar_y: f32) -> Rect {
    let x = (knob_x - CARD_W * 0.5).clamp(bar_x, (bar_x + bar_w - CARD_W).max(bar_x));
    Rect::new(x, bar_y - CARD_GAP - CARD_H, CARD_W, CARD_H)
}

/// Draw one resident still into the card. `tex` is the render cache's texture, `(tw, th)` its
/// decoded size.
pub fn draw(p: Painter, r: Rect, tex: u32, tw: f32, th: f32) {
    let uv = r.cover_uv(tw, th, plx_ui::Crop::Centre);
    p.tex_carded(tex, uv, r, CARD_RAD, theme::TINT_WHITE, 0.0);
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
    fn the_card_rides_the_knob_inside_the_bar() {
        let (bx, bw, by) = (96.0, 1728.0, 882.0);
        let mid = card_rect(960.0, bx, bw, by);
        assert_eq!(mid.x, 960.0 - CARD_W * 0.5);
        assert_eq!(mid.y + mid.h, by - CARD_GAP);
        assert_eq!(card_rect(100.0, bx, bw, by).x, bx, "clamped at the left end");
        assert_eq!(card_rect(1820.0, bx, bw, by).x, bx + bw - CARD_W, "clamped at the right end");
    }
}
