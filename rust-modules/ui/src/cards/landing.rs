//! Animated landings: a content change that no focus move announced (a card leaves, a card
//! arrives, the cards after them shift) plays over frames instead of cutting.
//!
//! What a section does with one, once it sees the cards it shows are not the cards it showed:
//!
//! - **a removed card** is a ghost: it stays at the slot it held, fading out. Its pixels come from
//!   the owner, which keeps the removed card itself ([`Shelf::paint_ghosts`](super::Shelf::paint_ghosts)
//!   hands it the ghosts' [`elem_key`](super::elem_key)s), because the current source no longer
//!   knows the item and a borrowed [`Art`](crate::widgets::Art) cannot be retained. A ghost is
//!   identified by its element, never by an index into any publication, so a later, unrelated
//!   publication cannot make it show another card. A ghost drawn at rest is not lifted; one that
//!   was the focused card starts at the scale it was drawn at and eases to rest while it fades;
//! - **a card that stays** starts where it was on screen and a spring carries it to its new slot.
//!   A slot is a point, `(x, y)`: a strip's slots share a `y`, a grid's do not. A card that keeps
//!   its row slides along it; one that **changes row** (crosses a line break) never flies across
//!   the grid: it fades out where it stood and in at its new slot, the two alphas complementary
//!   (`out = 1 - in`) on the fade's stiffness, the leaving copy drawn under the cards that slide
//!   ([`Landing::leaving`]). A diagonal flight crossed a whole row of tiles on its way. A card that
//!   only moves with the scroll (its slot's `y` did not change) keeps the plain 2-D spring: every
//!   card on screen moves the same way, so nothing crosses anything. There is no stagger: every
//!   survivor starts on the same frame. The offsets are taken against the scroll's own move for
//!   the focused card (the landing rule of `mod.rs` has already kept it at its slot, so its offset
//!   is 0), but the scroll is clamped to its range, so a focused card at an end of the strip can
//!   still move on screen;
//! - **an inserted card** fades in, each card on its own alpha.
//!
//! The owner names the layout through [`Scene::slot`] (index to the slot's top-left in the
//! unscrolled strip or document) and the scroll through [`Scene::shift`]; a ghost's `y` is held
//! as the same kind of shift, so a grid whose document scroll moved under a landing keeps the ghost
//! where it stood on screen.
//!
//! The first appearance of a section's cards is not a landing (nothing was there to change), nor is
//! a change while the section is [`dormant`](super::Shelf::dormant) or a slide of the window the
//! source holds (its `global` offset or `page_epoch` moved): all of those are adopted whole.
//!
//! **No cap.** Every card of the source is diffed (by element, through a map), but only cards whose
//! old or new slot is on-axis (plus the cull margin) get a spring, a fade or a ghost; the rest snap,
//! because nothing of them is on screen to animate.
//!
//! **After the menu, not under it.** A landing that a menu's choice started (the Watchlist's
//! Remove from the card's hold menu) does not step while a menu still veils the page
//! ([`page_veiled`](crate::containers::modal::page_veiled): a surface opening, open, or not yet
//! faded below [`CLOSING_GONE`](crate::containers::modal::CLOSING_GONE), where the menu has let
//! go of the host), and every spring starts together on the first frame after. Run under the
//! menu's fade, the tiles slid beneath its text, and their motion forced a page pass under the
//! menu's cached host on every frame of the fade (`popover::host_refresh`), and a glass menu
//! rebuilt its blur source from the moving page. While the landing waits under the veil it reports
//! no motion at all (the fading menu presents those frames). A landing no menu ever veiled starts
//! at once: the frame it begins in draws the start state (`fresh`: a grid does not step on it, so
//! the first drawn frame is the frame before the change), and the next one moves.
//!
//! **A menu may let the page reflow.** A surface whose body answers
//! `Screen::lets_host_reflow` (the Watchlist's Sort & Filter menu) casts no veil, so its choices'
//! landings play under it, with the host cache following the page live
//! (`popover::host::set_reflow`) and recaptured once when the cards stop.
//!
//! **Where cards stand while they move.** A card drawn over another is a bug at rest and in flight:
//! the grid paints the ghosts and leaving copies, then the still cards, then the cards fading in, then the movers by the
//! length of their way (old slot to new, fixed when the landing began, the longest on top), then the
//! focused card. A new card stays at alpha 0 while the cards in flight (their rects as they stand
//! this frame, the union of their areas) cover half its cell or more, and begins to fade in on the
//! first frame they cover less ([`Occupancy`]); it is then drawn under them, after the still cards.
//!
//! **Layout motion and the held page image.** These springs move layout, so they are not
//! `idle::decorative`: while they run the dispatcher keeps a page image held. A landing that
//! arrives while the page IS an image starts at once, so its first live frame shows the start
//! state (the old slots and a full-alpha ghost), but it does not step while the image is held
//! ([`page_image_held`](crate::containers::transition::page_image_held)); the stepping begins on
//! the first tick after the image is released.
//!
//! The springs step on the frame's own `dt` through [`Spring::step`](crate::Spring::step); the
//! Remove lands on the first feed after the hold menu's veil drops below `CLOSING_GONE`; no timer counts
//! the wait. No wall clock is read.
//! The state is transient render history: it is never written to the motion canon, and a
//! replay reconstructs it by running the same ticks.
//!
//! A settled or disabled section pays nothing per tick when the source's length and
//! [`revision`](super::CardSource::revision) did not move; a source with no revision pays one
//! comparison per card.

use std::collections::{HashMap, HashSet};

use plx_machine::machine::Host;

use super::pool::key;
use super::CardSource;
use crate::Spring;

/// A removed card still on screen.
#[derive(Clone, Copy)]
pub(crate) struct Ghost {
    /// The removed card's element, as [`key`] numbers it.
    pub key: u64,
    /// Its index when it was last adopted: where it stood in the strip.
    pub was: usize,
    /// How far the section's scroll moved since, px `(x, y)`: the ghost stands at its old slot plus
    /// this, which is where it stood on screen.
    pub shift: (f32, f32),
    /// Its alpha, 1 to 0.
    pub alpha: Spring,
    /// Its pop scale, from what it was drawn at to rest.
    pub lift: Spring,
    /// It was the focused card: it goes as one (`Tile::focused`), so a state's focused treatment
    /// does not snap to the unfocused one on the frame it leaves.
    pub focused: bool,
}

/// What a tick tells the landing about the section this frame.
pub(crate) struct Scene<'a> {
    /// How far the section moved its scroll this tick to keep its focused card where it was (px
    /// `(x, y)`, `(0, 0)` for none: the landing rule, clamped to the scroll's range).
    pub shift: (f32, f32),
    /// Slot `i`'s top-left in the strip (or document), unscrolled, px.
    pub slot: &'a dyn Fn(usize) -> (f32, f32),
    /// Slot `i`'s top-left as the cards were DRAWN before this tick: the same as [`slot`](Self::slot)
    /// unless the owner moved its layout in the tick (a grid's caption band left one row for another),
    /// when a card starts from where it stood on screen under the old layout and the layout's move
    /// plays on the landing's spring, with the card.
    pub was: &'a dyn Fn(usize) -> (f32, f32),
    /// Whether a card at this point in the strip (unscrolled) is on-axis now.
    pub on: &'a dyn Fn(f32, f32) -> bool,
    /// The cell that was focused before this tick and the scale it was drawn at.
    pub lifted: Option<(usize, f32)>,
}

/// A moving card's offset from its slot, px: two springs of one stiffness that start together, so
/// the card travels a straight line.
#[derive(Clone, Copy)]
struct Slide {
    x: Spring,
    y: Spring,
    /// How far the card travels in all, px squared (only the order matters): the length of its path from where it stood on screen
    /// to its new slot, fixed when the landing begins. The longer path draws on top
    /// ([`Landing::travel`]), and the rank never changes mid-flight.
    travel: f32,
}

impl Slide {
    fn settled(&self) -> bool {
        use plx_machine::idle::settled;
        settled(self.x.pos, 0.0, self.x.vel) && settled(self.y.pos, 0.0, self.y.vel)
    }
}

#[derive(Clone, Default)]
struct Anim {
    /// Cards the animation was built for: a source of another length is drawn without it until
    /// the next tick has diffed again.
    n: usize,
    /// The offset in px of each moving card (by [`key`]), springing to 0.
    off: HashMap<u64, Slide>,
    /// The alpha of each arriving card, springing to 1.
    fade: HashMap<u64, Spring>,
    /// Each card that changed row (by [`key`]): its leaving copy's offset from its new slot, px —
    /// where it stood on screen. The copy's alpha is `1 -` the card's [`fade`](Self::fade).
    wrap: HashMap<u64, (f32, f32)>,
    ghosts: Vec<Ghost>,
    /// The cell the focused card left as a ghost, now held by another card (the takeover): its
    /// pop is restarted from rest by the owner, once ([`Landing::take_takeover`]).
    takeover: Option<usize>,
}

impl Anim {
    fn retain_live(&mut self) {
        use plx_machine::idle::settled;
        self.off.retain(|_, s| !s.settled());
        self.fade.retain(|_, s| !settled(s.pos, 1.0, s.vel));
        let fade = &self.fade;
        self.wrap.retain(|k, _| fade.contains_key(k));
        self.ghosts.retain(|g| !settled(g.alpha.pos, 0.0, g.alpha.vel));
    }

    fn empty(&self) -> bool {
        self.off.is_empty() && self.fade.is_empty() && self.ghosts.is_empty()
    }
}

pub(crate) struct Landing {
    on: bool,
    /// The cards (element keys) the section last adopted.
    keys: Vec<u64>,
    /// Whether any of them had content (a section of placeholders has nothing to animate from).
    loaded: bool,
    /// The source's `(global(0), page_epoch)` and `revision` when the cards were last adopted or
    /// last found unchanged.
    window: (usize, u32),
    revision: Option<u64>,
    /// The scroll the section moved to keep its focused card in place since the cards last matched
    /// (the sum of the landing shifts of the ticks that saw the change).
    shift: (f32, f32),
    anim: Option<Anim>,
    /// The last [`step`](Self::step) did not advance anything (veiled, or under a held page image).
    paused: bool,
    /// The landing began in the tick that is now drawing and has not yet been stepped: its first
    /// step is skipped, so the first drawn frame is exactly the start state.
    fresh: bool,
    /// The springs of the landing now playing have stepped at least once.
    stepped: bool,
    /// The owner draws the frame a landing begins in as the start state ([`fresh`](Self::fresh)):
    /// a grid does, a shelf steps on it.
    hold_first: bool,
}

impl Landing {
    pub(crate) const fn new() -> Self {
        Self { on: false, keys: Vec::new(), loaded: false, window: (0, 0), revision: None, shift: (0.0, 0.0), anim: None, paused: false, fresh: false, stepped: false, hold_first: false }
    }

    pub(crate) const fn set_enabled(&mut self, on: bool) {
        self.on = on;
    }

    /// The frame a landing begins in only draws the start state; the springs step from the next.
    pub(crate) const fn hold_first_frame(&mut self, on: bool) {
        self.hold_first = on;
    }

    /// Take the cards as they are, with nothing to play (a first load, a dormant section, a slid window).
    fn adopt<H: Host, S: CardSource<H>>(&mut self, src: &S) {
        let n = src.len();
        self.keys.clear();
        self.keys.extend((0..n).map(|i| key(&src.elem(i))));
        self.loaded = (0..n).any(|i| src.loaded(i));
        self.window = (src.global(0), src.page_epoch());
        self.revision = src.revision();
        self.shift = (0.0, 0.0);
        self.anim = None;
        self.paused = false;
        self.fresh = false;
    }

    /// Take the cards as they are and drop whatever is playing (a dormant section).
    pub(crate) fn snap<H: Host, S: CardSource<H>>(&mut self, src: &S) {
        if self.on {
            self.adopt(src);
        }
    }

    /// Whether anything is still to play: an owner that keeps the removed cards for the ghosts
    /// keeps them until this is false.
    pub(crate) fn pending(&self) -> bool {
        self.anim.is_some()
    }

    /// Whether a landing has started but waits for the page image to go, so a frame must follow
    /// the release.
    pub(crate) fn held(&self) -> bool {
        self.anim.is_some() && crate::containers::transition::page_image_held()
    }

    /// A landing is playing but its last step did not run: the menu's veil has not yet dropped
    /// below `CLOSING_GONE`, or the page is a held image. An owner holds the motion it pairs with the
    /// landing (the takeover's pop) while this is true, so everything starts together.
    pub(crate) fn waiting(&self) -> bool {
        self.anim.is_some() && self.paused
    }

    /// A landing is playing and its springs have not moved yet: the veil or a held image has kept
    /// them, or the frame it began in is the start state still being drawn.
    pub(crate) fn unstepped(&self) -> bool {
        self.anim.is_some() && !self.stepped
    }

    /// The cell whose focused card left as a ghost and that another card now holds, once: the
    /// owner restarts that cell's pop from rest, so the takeover rises as the ghost sinks (one
    /// lifted tile at a time, never two side by side).
    pub(crate) fn take_takeover(&mut self) -> Option<usize> {
        self.anim.as_mut().and_then(|a| a.takeover.take())
    }

    /// Whether a takeover is still waiting for its owner to take it (the owner's focus can come
    /// back a frame after the card left: the app reconciles it then).
    pub(crate) fn has_takeover(&self) -> bool {
        self.anim.as_ref().is_some_and(|a| a.takeover.is_some())
    }

    /// The cards that changed row, of `n`: each one's index, its leaving copy's offset from its
    /// slot (px, where it stood on screen) and the copy's alpha (`1 -` the card's own).
    pub(crate) fn leaving(&self, n: usize) -> impl Iterator<Item = (usize, (f32, f32), f32)> + '_ {
        self.anim.as_ref().filter(|a| a.n == n && !a.wrap.is_empty()).into_iter().flat_map(move |a| {
            self.keys.iter().enumerate().filter_map(move |(i, k)| {
                let off = *a.wrap.get(k)?;
                let inn = a.fade.get(k).map_or(1.0, |s| s.pos.clamp(0.0, 1.0));
                Some((i, off, 1.0 - inn))
            })
        })
    }

    /// Ghosts still on screen.
    pub(crate) fn has_ghosts(&self) -> bool {
        self.anim.as_ref().is_some_and(|a| !a.ghosts.is_empty())
    }

    pub(crate) fn ghosts(&self) -> &[Ghost] {
        self.anim.as_ref().map_or(&[], |a| &a.ghosts)
    }

    /// Card `i`'s x offset of `n` cards, px.
    pub(crate) fn offset(&self, n: usize, i: usize) -> f32 {
        self.offset_xy(n, i).0
    }

    /// Card `i`'s offset of `n` cards, px `(x, y)`.
    pub(crate) fn offset_xy(&self, n: usize, i: usize) -> (f32, f32) {
        self.anim.as_ref().filter(|a| a.n == n && !a.off.is_empty())
            .and_then(|a| a.off.get(self.keys.get(i)?)).map_or((0.0, 0.0), |s| (s.x.pos, s.y.pos))
    }

    /// How far card `i` of `n` travels from where it stood to its slot, px squared, while it is moving
    /// (a survivor that slides): fixed when the landing began, so a draw order keyed on it holds
    /// still all flight. `None` for a card that is not moving.
    pub(crate) fn travel(&self, n: usize, i: usize) -> Option<f32> {
        self.anim.as_ref().filter(|a| a.n == n && !a.off.is_empty())
            .and_then(|a| a.off.get(self.keys.get(i)?)).map(|s| s.travel)
    }

    /// Card `i`'s alpha of `n` cards: 1 unless it is arriving.
    pub(crate) fn alpha(&self, n: usize, i: usize) -> f32 {
        self.anim.as_ref().filter(|a| a.n == n && !a.fade.is_empty())
            .and_then(|a| a.fade.get(self.keys.get(i)?)).map_or(1.0, |s| s.pos.clamp(0.0, 1.0))
    }

    /// Run before the section steps: diff the cards against the adopted ones and start what the
    /// change plays; whether it started one.
    pub(crate) fn observe<H: Host, S: CardSource<H>>(&mut self, src: &S, scene: &Scene<'_>) -> bool {
        if !self.on {
            return false;
        }
        let n = src.len();
        let revision = src.revision();
        if n == self.keys.len() && revision.is_some() && revision == self.revision {
            return false;
        }
        let same = n == self.keys.len() && (0..n).all(|i| key(&src.elem(i)) == self.keys[i]);
        let window = (src.global(0), src.page_epoch());
        if same {
            // a change that came and went leaves nothing to play
            self.revision = revision;
            self.window = window;
            self.shift = (0.0, 0.0);
            return false;
        }
        self.shift = (self.shift.0 + scene.shift.0, self.shift.1 + scene.shift.1);
        if self.keys.is_empty() || !self.loaded || window != self.window {
            self.adopt(src);
            return false;
        }
        self.begin(src, scene);
        true
    }

    /// The owner moved its layout (`scene.was` to `scene.slot`) while a landing plays and the cards did
    /// not change: every card, copy and ghost keeps the place it is drawn at, and the layout's move
    /// plays out on the springs that are already carrying them (a card at rest starts one).
    pub(crate) fn rebase(&mut self, n: usize, scene: &Scene<'_>) {
        let Some(a) = self.anim.as_mut().filter(|a| a.n == n && self.keys.len() == n) else { return };
        for (i, k) in self.keys.iter().enumerate() {
            let (was_at, now_at) = ((scene.was)(i), (scene.slot)(i));
            let d = (was_at.0 - now_at.0, was_at.1 - now_at.1);
            if d.0.abs() < 0.5 && d.1.abs() < 0.5 {
                continue;
            }
            if let Some(s) = a.off.get_mut(k) {
                s.x.pos += d.0;
                s.y.pos += d.1;
            } else if let Some(w) = a.wrap.get_mut(k) {
                // the leaving copy stays where it stood; the card itself fades in at its slot
                w.0 += d.0;
                w.1 += d.1;
            } else if (scene.on)(now_at.0, now_at.1) || (scene.on)(was_at.0, was_at.1) {
                a.off.insert(*k, Slide { x: Spring::at(d.0), y: Spring::at(d.1), travel: d.0 * d.0 + d.1 * d.1 });
            }
        }
        for g in &mut a.ghosts {
            let (was_at, now_at) = ((scene.was)(g.was), (scene.slot)(g.was));
            g.shift = (g.shift.0 + was_at.0 - now_at.0, g.shift.1 + was_at.1 - now_at.1);
        }
    }

    fn begin<H: Host, S: CardSource<H>>(&mut self, src: &S, scene: &Scene<'_>) {
        let n = src.len();
        let new: Vec<u64> = (0..n).map(|i| key(&src.elem(i))).collect();
        let old = std::mem::take(&mut self.keys);
        let mut old_at: HashMap<u64, usize> = HashMap::with_capacity(old.len());
        for (i, &k) in old.iter().enumerate() {
            old_at.entry(k).or_insert(i);
        }
        let kept: HashSet<u64> = new.iter().copied().collect();
        // where a card stood on screen is its old slot plus the scroll's move; the focused card,
        // which the landing rule kept in place, comes out at 0 and so does not move
        let shift = std::mem::take(&mut self.shift);
        let playing = self.anim.is_some();
        let mut prev = self.anim.take().unwrap_or_default();
        let mut next = Anim { n, ..Anim::default() };
        for (i, &k) in new.iter().enumerate() {
            let Some(&was) = old_at.get(&k) else {
                // arriving: a card still fading in, or a ghost brought back, goes on from its alpha
                let ghost = prev.ghosts.iter().position(|g| g.key == k).map(|g| prev.ghosts.swap_remove(g).alpha.pos);
                let from = prev.fade.get(&k).map(|s| s.pos).or(ghost);
                let (x, y) = (scene.slot)(i);
                if (scene.on)(x, y) {
                    next.fade.insert(k, Spring::at(from.unwrap_or(0.0)));
                }
                continue;
            };
            let carried = prev.off.get(&k).map_or((0.0, 0.0), |s| (s.x.pos, s.y.pos));
            let (was_at, now_at) = ((scene.was)(was), (scene.slot)(i));
            let start = (was_at.0 - now_at.0 + shift.0 + carried.0, was_at.1 - now_at.1 + shift.1 + carried.1);
            // off-axis both before and after: nothing of it is on screen to slide
            let seen = (scene.on)(now_at.0, now_at.1) || (scene.on)(now_at.0 + start.0, now_at.1 + start.1);
            // its row changed (under the layout that stands now: a caption band's move is not a row
            // change) AND it would move up or down on screen: it crossfades instead
            if seen && ((scene.slot)(was).1 - now_at.1).abs() >= 0.5 && start.1.abs() >= 0.5 {
                next.wrap.insert(k, start);
                next.fade.insert(k, Spring::at(0.0));
                continue;
            }
            if seen && (start.0.abs() >= 0.5 || start.1.abs() >= 0.5) {
                next.off.insert(k, Slide { x: Spring::at(start.0), y: Spring::at(start.1), travel: start.0 * start.0 + start.1 * start.1 });
            }
            if let Some(s) = prev.fade.get(&k) {
                next.fade.insert(k, *s);
            }
        }
        // ghosts still fading from an earlier landing stay where they stood on screen
        for mut g in prev.ghosts {
            if !kept.contains(&g.key) {
                g.shift = (g.shift.0 + shift.0, g.shift.1 + shift.1);
                next.ghosts.push(g);
            }
        }
        for (was, &k) in old.iter().enumerate().filter(|(_, k)| !kept.contains(k)) {
            let (at, now_at) = ((scene.was)(was), (scene.slot)(was));
            // a ghost stands where it stood on screen, under the layout the cards were drawn in
            let shift = (shift.0 + at.0 - now_at.0, shift.1 + at.1 - now_at.1);
            if (scene.on)(now_at.0 + shift.0, now_at.1 + shift.1) {
                let lifted = scene.lifted.filter(|&(c, _)| c == was);
                if lifted.is_some() {
                    next.takeover = Some(was);
                }
                let s = lifted.map_or(1.0, |(_, s)| s.max(1.0));
                next.ghosts.push(Ghost { key: k, was, shift, alpha: Spring::at(1.0), lift: Spring::at(s), focused: lifted.is_some() });
            }
        }
        // a landing over one still waiting waits with it
        if !playing {
            self.paused = false;
            self.stepped = false;
        }
        self.anim = (!next.empty()).then_some(next);
        self.fresh = self.hold_first && self.anim.is_some();
        self.keys = new;
        self.loaded = (0..n).any(|i| src.loaded(i));
        self.window = (src.global(0), src.page_epoch());
        self.revision = src.revision();
    }

    /// One frame of the animation. Nothing steps while the page is a held image (the first live
    /// frame after it shows the start state), nor while a menu veils the page (module doc). The
    /// frame a landing begins in only draws the start state (`fresh`), so a change that landed
    /// after the previous tick never shows its end before it moves. `occupy` is the grid's
    /// cells: an arriving card waits at alpha 0 until the moving survivors cover less than half of
    /// its cell (their rects as they stand after this step).
    pub(crate) fn step(&mut self, dt: f32, k: (f32, f32), occupy: Option<&Occupancy<'_>>) {
        let Some(a) = self.anim.as_mut() else { return };
        self.paused = true;
        if crate::containers::transition::page_image_held() || crate::containers::modal::page_veiled() {
            // a held image, or the menu's own fade presenting these frames: nothing of the page
            // moves, and what is drawn meanwhile IS the start state, so the first live step is
            // not skipped
            self.fresh = false;
            return;
        }
        self.paused = false;
        if std::mem::take(&mut self.fresh) {
            return;
        }
        self.stepped = true;
        for s in a.off.values_mut() {
            s.x.step(0.0, k.0, dt);
            s.y.step(0.0, k.0, dt);
        }
        // a card that has not begun to appear waits while the cards in flight cover half its cell
        // or more (their rects as they stand now: one that has not reached the cell yet does not hold it)
        let blocked: Vec<u64> = occupy.map_or_else(Vec::new, |o| {
            let index: HashMap<u64, usize> = self.keys.iter().enumerate().map(|(i, k)| (*k, i)).collect();
            let flying: Vec<Rect> = a
                .off
                .iter()
                .filter_map(|(k, s)| {
                    let at = o.slot_of(*index.get(k)?);
                    Some((at.0 + s.x.pos, at.1 + s.y.pos, o.size.0, o.size.1))
                })
                .collect();
            // (a card that changed row crossfades from where it stood: it is not new)
            a.fade
                .iter()
                .filter(|(k, s)| s.pos <= 0.0 && !a.wrap.contains_key(*k))
                .filter_map(|(k, _)| {
                    let at = o.slot_of(*index.get(k)?);
                    (covered_share((at.0, at.1, o.size.0, o.size.1), &flying) >= HALF).then_some(*k)
                })
                .collect()
        });
        for (key, s) in a.fade.iter_mut() {
            if !blocked.contains(key) {
                s.step(1.0, k.1, dt);
            }
        }
        for g in a.ghosts.iter_mut() {
            g.alpha.step(0.0, k.1, dt);
            g.lift.step(1.0, k.1, dt);
        }
        a.retain_live();
        if a.empty() {
            self.anim = None;
        }
    }
}

/// The grid's cells, for [`Landing::step`]'s new-card hold: where card `i` stands (document
/// px) and how large a card is.
pub(crate) struct Occupancy<'a> {
    pub slot: &'a dyn Fn(usize) -> (f32, f32),
    pub size: (f32, f32),
}

/// A rect as `(x, y, w, h)`, px.
type Rect = (f32, f32, f32, f32);

/// The share of a new card's cell that the cards in flight must cover to hold it at alpha 0: it
/// begins to appear on the first frame they cover less than this.
const HALF: f32 = 0.5;

/// The share of `cell` that `others` cover together, 0 to 1: the area of their union inside the cell
/// (two cards in flight may cross each other, so their areas are not summed).
fn covered_share(cell: Rect, others: &[Rect]) -> f32 {
    let (cx1, cy1) = (cell.0 + cell.2, cell.1 + cell.3);
    let inside: Vec<(f32, f32, f32, f32)> = others
        .iter()
        .map(|r| (r.0.max(cell.0), r.1.max(cell.1), (r.0 + r.2).min(cx1), (r.1 + r.3).min(cy1)))
        .filter(|r| r.2 > r.0 && r.3 > r.1)
        .collect();
    if inside.is_empty() || cell.2 <= 0.0 || cell.3 <= 0.0 {
        return 0.0;
    }
    let mut xs: Vec<f32> = inside.iter().flat_map(|r| [r.0, r.2]).collect();
    let mut ys: Vec<f32> = inside.iter().flat_map(|r| [r.1, r.3]).collect();
    xs.sort_by(f32::total_cmp);
    ys.sort_by(f32::total_cmp);
    let mut area = 0.0;
    for xw in xs.windows(2) {
        for yw in ys.windows(2) {
            let (mx, my) = ((xw[0] + xw[1]) * 0.5, (yw[0] + yw[1]) * 0.5);
            if inside.iter().any(|r| r.0 <= mx && mx < r.2 && r.1 <= my && my < r.3) {
                area += (xw[1] - xw[0]) * (yw[1] - yw[0]);
            }
        }
    }
    area / (cell.2 * cell.3)
}

impl Occupancy<'_> {
    fn slot_of(&self, i: usize) -> (f32, f32) {
        (self.slot)(i)
    }
}
