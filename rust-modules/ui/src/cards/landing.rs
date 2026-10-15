//! Animated landings: a content change that no focus move announced (a card leaves, a card
//! arrives, the cards after them shift) plays over frames instead of cutting.
//!
//! What a section does with one, once it sees the cards it shows are not the cards it showed:
//!
//! - **a removed card** is a ghost: it stays at the slot it held, drawn at rest (never lifted),
//!   fading out. Its pixels come from the PREVIOUS source ([`CardSource`] handed to
//!   `paint_ghosts`), because the current source no longer knows the item and a borrowed
//!   [`Art`](crate::widgets::Art) cannot be retained;
//! - **a card that stays** starts where it was and a spring carries it to its new slot. The
//!   offsets are taken against the focused card's own move, so the FOCUSED tile's offset is
//!   exactly 0 and its rect never changes (the landing rule of `mod.rs` has already kept it where it
//!   was on screen);
//! - **an inserted card** fades in.
//!
//! The first appearance of a section's cards is not a landing (nothing was there to change), nor is
//! a change while the section is [`dormant`](super::Shelf::dormant): both are adopted whole.
//!
//! **Layout motion and the held page image.** These springs move layout, so they are not
//! `idle::decorative`: while they run the dispatcher keeps a page image held. A landing that
//! arrives while the page IS an image is therefore never started under it. It is recorded (the
//! section keeps showing the cards it last adopted), and when the image is released it is either a
//! snap, if a capture was taken after the landing (the image already shows the new cards), or an
//! animation that starts on the first live frame (the image shows the old cards, so the change
//! plays from them). The facts come from `plx_machine::idle::{page_image_held, page_captures}`.
//!
//! Time is the frame's own `dt` through [`Spring::step`](crate::Spring::step); nothing reads a
//! clock. The state is transient render history: it is never written to the motion canon, and a
//! replay reconstructs it by running the same ticks.
//!
//! Work is bounded by [`MAX_ROW_ITEMS`] cards, and a settled or disabled section pays one
//! comparison per card per tick and allocates nothing.

use plx_machine::machine::Host;

use super::pool::key;
use super::CardSource;
use crate::card_row::MAX_ROW_ITEMS;
use crate::Spring;

/// A removed card still on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ghost {
    /// Its index in the PREVIOUS source (what `paint_ghosts` reads the card from).
    pub was: usize,
    /// How far the section's scroll moved when the cards changed, px: the ghost stands at its old
    /// slot plus this, which is where it stood on screen.
    pub shift: f32,
}

#[derive(Clone)]
struct Anim {
    /// Cards the animation was built for: a source of another length is drawn without it until
    /// the next tick has diffed again.
    n: usize,
    /// Each current card's x offset in px, springing to 0.
    off: Vec<Spring>,
    /// Which current cards are new.
    fresh: Vec<bool>,
    ghosts: Vec<Ghost>,
    /// The alpha of every new card (0 to 1) and of every ghost (1 to 0).
    a_in: Spring,
    a_out: Spring,
}

impl Anim {
    fn settled(&self) -> bool {
        use plx_machine::idle::settled;
        self.off.iter().all(|s| settled(s.pos, 0.0, s.vel))
            && (self.fresh.iter().all(|f| !f) || settled(self.a_in.pos, 1.0, self.a_in.vel))
            && (self.ghosts.is_empty() || settled(self.a_out.pos, 0.0, self.a_out.vel))
    }
}

pub(crate) struct Landing {
    on: bool,
    /// The cards (element keys) the section last adopted.
    keys: Vec<u64>,
    /// Whether any of them had content (a section of placeholders has nothing to animate from).
    loaded: bool,
    /// A change was seen under a held page image: the `idle::page_captures` count when the cards
    /// now showing were first seen, and a fingerprint of them (a newer landing starts the count
    /// over).
    deferred: Option<(u32, u64)>,
    /// The scroll the section moved to keep its focused card in place since the cards last matched
    /// (the sum of the landing shifts of the ticks that saw the change).
    shift: f32,
    anim: Option<Anim>,
}

impl Landing {
    pub(crate) const fn new() -> Self {
        Self { on: false, keys: Vec::new(), loaded: false, deferred: None, shift: 0.0, anim: None }
    }

    pub(crate) const fn set_enabled(&mut self, on: bool) {
        self.on = on;
    }

    /// Take the cards as they are, with nothing to play (a first load, a dormant section).
    fn adopt<H: Host, S: CardSource<H>>(&mut self, src: &S) {
        let n = src.len().min(MAX_ROW_ITEMS);
        self.keys.clear();
        self.keys.extend((0..n).map(|i| key(&src.elem(i))));
        self.loaded = (0..n).any(|i| src.loaded(i));
        self.deferred = None;
        self.shift = 0.0;
        self.anim = None;
    }

    /// Take the cards as they are and drop whatever is playing (a dormant section).
    pub(crate) fn snap<H: Host, S: CardSource<H>>(&mut self, src: &S) {
        if self.on {
            self.adopt(src);
        }
    }

    /// Whether anything is still to play or waiting for the page image to go: an owner that
    /// keeps the previous source alive for the ghosts keeps it until this is false.
    pub(crate) fn pending(&self) -> bool {
        self.anim.is_some() || self.deferred.is_some()
    }

    /// Ghosts still on screen (an owner keeps the previous source for them).
    pub(crate) fn has_ghosts(&self) -> bool {
        self.anim.as_ref().is_some_and(|a| !a.ghosts.is_empty())
    }

    pub(crate) fn ghosts(&self) -> &[Ghost] {
        self.anim.as_ref().map_or(&[], |a| &a.ghosts)
    }

    pub(crate) fn ghost_alpha(&self) -> f32 {
        self.anim.as_ref().map_or(0.0, |a| a.a_out.pos.clamp(0.0, 1.0))
    }

    /// Card `i`'s x offset of `n` cards, px.
    pub(crate) fn offset(&self, n: usize, i: usize) -> f32 {
        self.anim.as_ref().filter(|a| a.n == n).and_then(|a| a.off.get(i)).map_or(0.0, |s| s.pos)
    }

    /// Card `i`'s alpha of `n` cards: 1 unless it is arriving.
    pub(crate) fn alpha(&self, n: usize, i: usize) -> f32 {
        self.anim.as_ref().filter(|a| a.n == n && a.fresh.get(i).copied().unwrap_or(false))
            .map_or(1.0, |a| a.a_in.pos.clamp(0.0, 1.0))
    }

    /// Run before the section steps: diff the cards against the adopted ones and start what the
    /// change plays. `shift` is how far the section moved its scroll this tick to keep its focused
    /// card where it was (px, 0 for none: the landing rule, clamped to the scroll's range),
    /// `pitch` the slot width in px, and `snap` that the section is dormant (adopt, play nothing).
    pub(crate) fn observe<H: Host, S: CardSource<H>>(&mut self, src: &S, shift: f32, pitch: f32, snap: bool) {
        if !self.on {
            return;
        }
        let n = src.len().min(MAX_ROW_ITEMS);
        let same = n == self.keys.len() && (0..n).all(|i| key(&src.elem(i)) == self.keys[i]);
        if same {
            // a change that came and went under a held image leaves nothing to play
            self.deferred = None;
            self.shift = 0.0;
            return;
        }
        self.shift += shift;
        if snap || self.keys.is_empty() || !self.loaded {
            return self.adopt(src);
        }
        let print = (0..n).fold(n as u64, |h, i| h.rotate_left(7) ^ key(&src.elem(i)));
        if plx_machine::idle::page_image_held() {
            if self.deferred.map(|(_, p)| p) != Some(print) {
                self.deferred = Some((plx_machine::idle::page_captures(), print));
            }
            return;
        }
        if self.deferred.take().is_some_and(|(seen, p)| p == print && plx_machine::idle::page_captures() != seen) {
            // the image the page was held as was captured after the change: it shows these cards
            return self.adopt(src);
        }
        self.begin(src, n, pitch);
    }

    fn begin<H: Host, S: CardSource<H>>(&mut self, src: &S, n: usize, pitch: f32) {
        let new: Vec<u64> = (0..n).map(|i| key(&src.elem(i))).collect();
        let old = std::mem::take(&mut self.keys);
        let at_old = |k: u64| old.iter().position(|&o| o == k);
        // where a card stood on screen is its old slot plus the scroll's move; the focused card,
        // which the landing rule kept in place, comes out at 0 and so does not move
        let shift = std::mem::take(&mut self.shift);
        let carried = |was: usize| self.anim.as_ref().and_then(|a| a.off.get(was)).map_or(0.0, |s| s.pos);
        let mut off = Vec::with_capacity(n);
        let mut fresh = Vec::with_capacity(n);
        for (i, &k) in new.iter().enumerate() {
            let was = at_old(k);
            fresh.push(was.is_none());
            let start = match was {
                Some(w) => (w as f32 - i as f32) * pitch + shift + carried(w),
                None => 0.0,
            };
            off.push(Spring::at(if start.abs() < 0.5 { 0.0 } else { start }));
        }
        let ghosts = old.iter().enumerate().filter(|(_, o)| !new.contains(o))
            .map(|(was, _)| Ghost { was, shift }).collect();
        self.anim = Some(Anim { n: src.len(), off, fresh, ghosts, a_in: Spring::at(0.0), a_out: Spring::at(1.0) });
        self.keys = new;
        self.loaded = (0..n).any(|i| src.loaded(i));
        self.deferred = None;
    }

    /// One frame of the animation.
    pub(crate) fn step(&mut self, dt: f32, k: (f32, f32)) {
        let Some(a) = self.anim.as_mut() else { return };
        for s in a.off.iter_mut() {
            s.step(0.0, k.0, dt);
        }
        if a.fresh.iter().any(|&f| f) {
            a.a_in.step(1.0, k.1, dt);
        }
        if !a.ghosts.is_empty() {
            a.a_out.step(0.0, k.1, dt);
        }
        if a.settled() {
            self.anim = None;
        }
    }
}
