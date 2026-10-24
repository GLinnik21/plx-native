//! Draw-class OVERDRAW ledger and draw-class MASK — the attribution instrument for the 88.6% of
//! the frame that is not the glass.
//!
//! `docs/backdrop-blur-profiling.md` established that the whole frame rasterizes **3.65 quads per
//! screen pixel** and that the glass is only 11.4% of it. That number came from the GPU's global
//! `FRAG_QUADS_RAST`, which cannot say *whose* quads they are: the wayland compositor draws into
//! the same counters, and one full-screen composite is a quarter of the budget on its own. This
//! module answers the three questions that follow, and none of them needs a GPU counter:
//!
//! 1. **What does the APP submit?** [`gate`] sums, per draw class, the screen-visible area of every
//!    quad the app actually issues, on the CPU, exactly, with one exception: `draw_ambient` books
//!    its quad before it decides to draw a flat ground as a clear, so a wash of one colour is booked
//!    as `ambient` fragments though none is shaded (`OVERDRAW` over-reports `ambient` on those
//!    frames; HWCNT does not). `/tmp/plxnative-overdraw` arms it and it logs one `OVERDRAW` line a
//!    second. This is immune to the two traps that make per-phase HWCNT
//!    cycle counts a bad budget — it is not `glFinish`-serialised and it cannot be billed for
//!    another process's work.
//! 2. **What does a class COST?** `/tmp/plxnative-drawmask=<classes>` refuses every draw of the
//!    named classes, so a whole-frame `frame.ui` HWCNT A/B against the unmasked control prices that
//!    class *as the frame sees it*, un-serialised. `drawmask=all` draws nothing at all and is
//!    therefore the **compositor floor**: whatever `frame.ui` still costs with an empty page is
//!    work this app cannot remove. The `card` class also answers to three PARTS, [`CardPart`]
//!    (`card.interior`, `card.ring`, `card.focus`), which refuse only that part of the card
//!    compositor's work, so a `card` delta can be split between the pieces that make it up.
//!
//! 3. **How is a card DRAWN?** The same armed ledger also books every `Class::Card` draw by the
//!    path the compositor took ([`CardPath`]: split into pieces, or whole and why).
//!    [`note_card_path`] books the quad's visible area once per path and the window line prints
//!    them as `cardpath`. The class
//!    ledger above counts logical composites and cannot see this (a split card is one composite and
//!    five GL draws); this can, and it is how a stalled spring that kept every visited card on the
//!    focus program was found. `/tmp/plxnative-nocardsplit` ([`set_card_split`]) switches the split
//!    off for a leg, so one binary measures it on and off interleaved.
//!
//! A masked leg is deliberately a BROKEN PICTURE. This is a measurement knob, not a feature — the
//! mask exists so a control-leg difference can be taken, and every leg it produces except the
//! control is unshippable by construction.
//!
//! All three surfaces are dev-only: with `devtriggers` off, [`gate`] is `false`, [`masked_part`] is
//! `false`, and [`note_px`], [`note_card_path`] and [`set_card_split`] are empty at compile time (and
//! [`card_split`] is always `true`: a release build always splits), so a release binary carries
//! neither the branch nor the counters.

/// What kind of quad a draw call submits. One variant per PRIMITIVE FAMILY rather than per screen
/// element, because that is the granularity `gfx`/`text` can label a draw at without every screen
/// having to declare itself — and it is enough to separate the things that plausibly differ in
/// cost per pixel (a four-tap blur pass, a textured card composite, a flat SDF fill).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// `draw_ambient` — the OPAQUE four-corner page wash. A flat ground (four equal corners, no
    /// ink) is drawn as a clear but booked here as the quad it would have been, so the ledger
    /// over-reports this class on those frames; `drawmask=ambient` refuses the clear along with
    /// the quad, so a mask A/B prices it correctly.
    Ambient = 0,
    /// `draw_grad4`/`draw_art_scrim` — translucent hero gradients and artwork bottom scrims.
    Grad = 1,
    /// `draw_rect`/`draw_rrect` and their sheened twins — every SDF fill, focus glow included.
    Rect = 2,
    /// `draw_shadow` — the standalone soft drop-shadow pass (the profile chip; cards fold theirs).
    Shadow = 3,
    /// `draw_tex_carded` — the folded card composite (texture + rim + penumbra), i.e. every art tile.
    Card = 4,
    /// `draw_tex`/`draw_tex_stroked` — any other textured quad (icons, masks, backdrop art).
    Image = 5,
    /// `text::draw_text*` — one quad per cached glyph STRING.
    Text = 6,
    /// `fs_glass` — the backdrop panel composite.
    Glass = 7,
    /// The blur chain's own FBO passes, measured in TARGET pixels (they do not touch the panel).
    Blur = 8,
    /// `gfx::draw_field` — the magnified UNDERLAY FIELD, plus the reduction chain that produces
    /// it, booked in TARGET pixels like [`Blur`](Self::Blur). Unlike `Blur` this one IS maskable,
    /// and masking it removes the whole feature at once: `gfx::field_kick` refuses
    /// before the chain runs, so a `drawmask=field` leg prices the sample and the draw together
    /// rather than leaving the draw sampling a stale texture.
    Field = 9,
}

pub const NCLASS: usize = 10;

/// The name each class answers to in `/tmp/plxnative-drawmask`, in [`Class`] order.
pub const NAMES: [&str; NCLASS] = [
    "ambient", "grad", "rect", "shadow", "card", "image", "text", "glass", "blur", "field",
];

/// How the compositor drew one `Class::Card` draw (an art tile), as booked by [`note_card_path`].
/// Every card the ledger would book as a logical `card` composite falls in exactly one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardPath {
    /// A resting card drawn in five scissored pieces: the interior through the shader's plain-texture
    /// exit, the ring through the full card math.
    Split = 0,
    /// A focused or risen card (`f > 0` or `dy > 0`): the focus program, the whole quad, unsplit.
    Focus = 1,
    /// A resting card on a reduced-resolution source pass (`CLIP_TARGET`): whole.
    Clipped = 2,
    /// A resting card with no certainly-plain interior (a circle, a sliver): whole.
    NoInterior = 3,
    /// A resting card whose corner radius is under half a pixel: the shader's whole-quad plain
    /// branch already, so there is nothing to cut.
    Plain = 4,
    /// A resting card that could have been split, with the split switched off for this leg.
    Off = 5,
}

pub const NPATH: usize = 6;

/// The name each path answers to on the window line, in [`CardPath`] order.
pub const PATH_NAMES: [&str; NPATH] = ["split", "focus", "clip", "nointerior", "plain", "off"];

/// A PART of the card class's work that `/tmp/plxnative-drawmask` can refuse on its own, beside the
/// whole classes of [`Class`]. They exist to split a `card` delta into its pieces on the TV: the
/// whole class is one number, and what to do about it (cut the interior's texture cost, bake the
/// ring, retire the focus program for a spring that has all but landed) depends on which part holds
/// it. A part is named by the card-path ledger's own buckets ([`CardPath`]) so the window line says
/// how many cards each one covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardPart {
    /// The interior piece of every SPLIT card ([`CardPath::Split`]): the one draw that leaves
    /// through the shader's plain-texture exit. The card's ring is still drawn.
    Interior = 0,
    /// The four ring pieces of every SPLIT card: the rim, the anti-aliased edge and the shadow's
    /// pad, through the full card math. The card's interior is still drawn.
    Ring = 1,
    /// Every card drawn whole through the focus program ([`CardPath::Focus`], whatever its pop
    /// factor: the focused card, and every card whose spring has not landed exactly).
    Focus = 2,
}

pub const NPART: usize = 3;

/// The name each part answers to in `/tmp/plxnative-drawmask`, in [`CardPart`] order. The dot is
/// part of the word: `card` masks the whole class, `card.ring` only the ring of the split cards.
pub const PART_NAMES: [&str; NPART] = ["card.interior", "card.ring", "card.focus"];

// The mask is one word: a bit per class, then a bit per part.
const _: () = assert!(NCLASS + NPART <= u32::BITS as usize);

/// The dev arm: the real ledger and the real mask. Its release twin sits below, same names, same
/// shape, doing nothing — and the `pub use` under it re-exports whichever one is compiled.
#[cfg(feature = "devtriggers")]
mod imp {
    use super::{CardPart, CardPath, Class, NAMES, NCLASS, NPATH, PART_NAMES, PATH_NAMES};
    use plx_base::eventlog::log;
    use plx_base::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};
    use std::cell::Cell;

    thread_local! {
        /// Bit per class, then (from bit `NCLASS`) bit per [`CardPart`]: refuse every draw of that
        /// class, or that part of the card class's work. Zero = the shipped picture.
        static MASK: Cell<u32> = const { Cell::new(0) };
        /// Is the ledger armed? Separate from the mask so a masked leg can also be counted.
        static ON: Cell<bool> = const { Cell::new(false) };
        /// `Painter::clip`'s live box in AUTHORED coordinates, or `None` for the whole canvas.
        static CLIP: Cell<Option<[f32; 4]>> = const { Cell::new(None) };
        /// PER-CLASS cells, not one `Cell<[f64; NCLASS]>`. An array cell has to be read and
        /// written WHOLE — 72 + 36 bytes copied twice for every booked draw — and the DIRECTION of
        /// that cost is what makes it a measurement problem rather than a micro-optimisation: a
        /// `drawmask` leg books fewer draws than its control, so it pays less bookkeeping, IN THE
        /// SAME DIRECTION as the saving it is trying to price. This module's whole claim is that
        /// it prices a class without perturbing it. Eight bytes per draw, and the bias is gone.
        static PX: [Cell<f64>; NCLASS] = const { [const { Cell::new(0.0) }; NCLASS] };
        static DRAWS: [Cell<u32>; NCLASS] = const { [const { Cell::new(0) }; NCLASS] };
        static FRAMES: Cell<u32> = const { Cell::new(0) };
        /// The card-path ledger: visible px and draws per [`CardPath`], per-path cells for the same
        /// reason as `PX`.
        static PATH_PX: [Cell<f64>; NPATH] = const { [const { Cell::new(0.0) }; NPATH] };
        static PATH_DRAWS: [Cell<u32>; NPATH] = const { [const { Cell::new(0) }; NPATH] };
        /// Is the resting-card split allowed? On unless a leg switched it off.
        static SPLIT: Cell<bool> = const { Cell::new(true) };
    }

    /// Switch the resting-card split off (`false`) or back on, for an A/B on one binary
    /// (`/tmp/plxnative-nocardsplit`). Off, a card that could be split is drawn whole and booked
    /// as [`CardPath::Off`].
    pub fn set_card_split(on: bool) {
        SPLIT.with(|f| f.set(on));
    }

    /// Whether the resting-card split is allowed (the shipped answer: always).
    #[inline]
    pub fn card_split() -> bool {
        SPLIT.with(|f| f.get())
    }

    /// Book one card draw against the path it took: the quad's screen-visible area (the very
    /// arithmetic [`gate`] books a class with) and one draw. Unlike [`gate`] this INCLUDES a blur
    /// source pass's draws, because those are the ones [`CardPath::Clipped`] exists to count; the
    /// class ledger keeps leaving them out, so the paths' draws can exceed the `card` class's.
    #[inline]
    pub fn note_card_path(path: CardPath, x: f32, y: f32, w: f32, h: f32) {
        if ON.with(|f| f.get()) {
            let i = path as usize;
            PATH_PX.with(|f| f[i].set(f[i].get() + visible_px(x, y, w, h)));
            PATH_DRAWS.with(|f| f[i].set(f[i].get() + 1));
        }
    }

    /// The area of the quad that can reach the panel: clipped to the canvas and to `Painter::clip`'s
    /// live box. What both ledgers book.
    #[inline]
    fn visible_px(x: f32, y: f32, w: f32, h: f32) -> f64 {
        let (mut x0, mut y0, mut x1, mut y1) = (
            x.max(0.0),
            y.max(0.0),
            (x + w).min(SCR_W),
            (y + h).min(SCR_H),
        );
        if let Some(cl) = CLIP.with(|f| f.get()) {
            x0 = x0.max(cl[0]);
            y0 = y0.max(cl[1]);
            x1 = x1.min(cl[2]);
            y1 = y1.min(cl[3]);
        }
        ((x1 - x0).max(0.0) * (y1 - y0).max(0.0)) as f64
    }

    /// Arm the ledger (`/tmp/plxnative-overdraw`).
    pub fn set_ledger(on: bool) {
        ON.with(|f| f.set(on));
        if on {
            log("OVERDRAW ledger on: per-class submitted quad area, logged once a second");
        }
    }

    /// Arm the mask from a trigger body: class names (`card`) and card part names (`card.ring`)
    /// separated by anything that is neither alphanumeric nor a dot, or `all`. A dot joins a word,
    /// so a part name is one name and `card.` or `ring` alone is an unknown one. An unknown name is
    /// reported rather than silently ignored — a leg that measured the control twice reads exactly
    /// like "this class costs nothing".
    pub fn set_mask(spec: &str) {
        let mut mask = 0u32;
        let mut bad: Vec<&str> = Vec::new();
        for word in spec
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
            .filter(|w| !w.is_empty())
        {
            if word.eq_ignore_ascii_case("all") {
                // Every class. A part is a piece of the card class, so `all` already refuses it.
                mask |= (1 << NCLASS) - 1;
            } else if word.eq_ignore_ascii_case(NAMES[Class::Blur as usize]) {
                log("OVERDRAW drawmask: \"blur\" is accounting-only; mask \"glass\" to remove the whole chain");
            } else if let Some(i) = NAMES.iter().position(|n| n.eq_ignore_ascii_case(word)) {
                mask |= 1 << i;
            } else if let Some(i) = PART_NAMES.iter().position(|n| n.eq_ignore_ascii_case(word)) {
                mask |= 1 << (NCLASS + i);
            } else {
                bad.push(word);
            }
        }
        if !bad.is_empty() {
            log(&format!(
                "OVERDRAW drawmask: unknown class {:?}; valid names: {} {} all",
                bad,
                NAMES.join(" "),
                PART_NAMES.join(" ")
            ));
        }
        MASK.with(|f| f.set(mask));
        let on: Vec<&str> = NAMES
            .iter()
            .chain(PART_NAMES.iter())
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, n)| *n)
            .collect();
        log(&format!(
            "OVERDRAW drawmask=0x{mask:x} skipping: {}",
            if on.is_empty() {
                "(nothing)".to_string()
            } else {
                on.join(",")
            }
        ));
    }

    /// Record `Painter::clip`'s live box, so the ledger counts the fragments a scissored quad can
    /// actually produce rather than the quad it declared.
    #[inline]
    pub fn set_clip(box_: Option<[f32; 4]>) {
        CLIP.with(|f| f.set(box_));
    }

    /// Is this class masked out? The REFUSAL half of [`gate`], with no booking.
    ///
    /// Exists for one caller: a surface whose mask leg has to be decided EARLY (before it does
    /// bookkeeping the leg is meant to remove) but whose ledger entry must be booked LATE, after
    /// every path that returns without drawing. Asking `gate` twice would double-count; asking it
    /// once, early, books quads that never rasterize.
    #[inline]
    pub fn masked(c: Class) -> bool {
        c != Class::Blur && MASK.with(|f| f.get()) & (1 << (c as usize)) != 0
    }

    /// Is this PART of the card class's work masked out (`card.interior`, `card.ring`,
    /// `card.focus`)? Asked only by the card compositor, AFTER [`gate`] has booked the card's
    /// logical composite and [`note_card_path`] the path it took, so a part-masked leg books exactly
    /// what its control books and only the GPU work differs. (A card whose whole class is masked
    /// never gets this far.)
    #[inline]
    pub fn masked_part(p: CardPart) -> bool {
        MASK.with(|f| f.get()) & (1 << (NCLASS + p as usize)) != 0
    }

    /// Should this draw be REFUSED, and if not, book it. One call per primitive, immediately after
    /// that primitive's own `gfx::culled` test so a culled quad is never counted.
    #[inline]
    pub fn gate(c: Class, x: f32, y: f32, w: f32, h: f32) -> bool {
        let i = c as usize;
        // [`Class::Blur`] is ACCOUNTING-ONLY and books itself through [`note_px`]: its passes write
        // reduced-resolution FBOs, so their authored-coordinate quad is meaningless here, and half
        // a masked chain would leave the panel sampling a stale texture rather than measuring
        // anything. The whole glass — chain included — is masked at `Class::Glass` instead.
        if c == Class::Blur {
            return false;
        }
        if MASK.with(|f| f.get()) & (1 << i) != 0 {
            return true;
        }
        // A blur source pass draws the page a second time into a small target; its quads are not
        // on the panel and would double every class in the ledger.
        if ON.with(|f| f.get()) && !crate::gfx::blur_source_pass() {
            add(i, visible_px(x, y, w, h));
        }
        false
    }

    /// Book `px` fragments against `c` directly — for a pass whose quad is not in authored
    /// coordinates at all (the blur chain writes reduced-resolution FBOs).
    #[inline]
    pub fn note_px(c: Class, px: f64) {
        if ON.with(|f| f.get()) {
            add(c as usize, px);
        }
    }

    #[inline]
    fn add(i: usize, px: f64) {
        PX.with(|f| f[i].set(f[i].get() + px));
        DRAWS.with(|f| f[i].set(f[i].get() + 1));
    }

    /// One PRESENTED frame has ended. Logs a window every `LOG_EVERY` frames and resets.
    pub fn frame_end() {
        if !ON.with(|f| f.get()) {
            return;
        }
        const LOG_EVERY: u32 = 60;
        let n = FRAMES.with(|f| {
            let n = f.get() + 1;
            f.set(n);
            n
        });
        if n < LOG_EVERY {
            return;
        }
        log(&flush_window());
    }

    /// Close the window: the `OVERDRAW` line for the frames since the last one, and every cell back
    /// to zero. Each class prints `name=px/draws` and each card path `name=px/draws` after
    /// `cardpath`, all per frame.
    pub(super) fn flush_window() -> String {
        let n = FRAMES.with(|f| f.replace(0));
        let px: [f64; NCLASS] = PX.with(|f| std::array::from_fn(|i| f[i].replace(0.0)));
        let draws: [u32; NCLASS] = DRAWS.with(|f| std::array::from_fn(|i| f[i].replace(0)));
        let path_px: [f64; NPATH] = PATH_PX.with(|f| std::array::from_fn(|i| f[i].replace(0.0)));
        let path_draws: [u32; NPATH] = PATH_DRAWS.with(|f| std::array::from_fn(|i| f[i].replace(0)));
        let frames = n.max(1) as f64;
        let screen = (SCR_W * SCR_H) as f64;
        // `blur` writes reduced-resolution targets, so it is NOT part of the panel's overdraw
        // ratio; it is reported beside it rather than inside it.
        let panel: f64 = (0..NCLASS)
            .filter(|i| *i != Class::Blur as usize)
            .map(|i| px[i])
            .sum();
        let per = |i: usize| px[i] / frames;
        let parts: Vec<String> = (0..NCLASS)
            .map(|i| {
                format!(
                    "{}={:.0}/{}",
                    NAMES[i],
                    per(i),
                    (draws[i] as f64 / frames + 0.5) as u32
                )
            })
            .collect();
        // Draws to one decimal: a path that fires on one frame in ten still shows.
        let paths: Vec<String> = (0..NPATH)
            .map(|i| format!("{}={:.0}/{:.1}", PATH_NAMES[i], path_px[i] / frames, path_draws[i] as f64 / frames))
            .collect();
        format!(
            "OVERDRAW frames={n} panel={:.0}px x{:.2} | {} | cardpath {}",
            panel / frames,
            panel / frames / screen,
            parts.join(" "),
            paths.join(" ")
        )
    }
}

/// The release arm: every entry point is present, `#[inline]`, and does nothing — so a release
/// binary carries neither the branch nor the counters, and every call site compiles unchanged.
///
/// A MODULE rather than seven `#[cfg(not(…))]` + `#[inline]` attribute pairs standing loose at
/// file scope, which is the shape this repository has been bitten by twice: a function inserted
/// between such a pair captures the attribute meant for its neighbour, nothing warns, and the only
/// symptom is codegen (`ff.rs`'s `stream_index` lost its `#[inline]` exactly that way). One `cfg`
/// on the module and one re-export cannot be split by an insertion, and the two arms now have the
/// same shape as each other.
#[cfg(not(feature = "devtriggers"))]
mod imp {
    use super::{CardPart, CardPath, Class};

    #[inline]
    pub fn gate(_c: Class, _x: f32, _y: f32, _w: f32, _h: f32) -> bool {
        false
    }
    #[inline]
    pub fn masked(_c: Class) -> bool {
        false
    }
    #[inline]
    pub fn masked_part(_p: CardPart) -> bool {
        false
    }
    #[inline]
    pub fn note_px(_c: Class, _px: f64) {}
    #[inline]
    pub fn set_clip(_box_: Option<[f32; 4]>) {}
    #[inline]
    pub fn frame_end() {}
    #[inline]
    pub fn set_ledger(_on: bool) {}
    #[inline]
    pub fn set_mask(_spec: &str) {}
    #[inline]
    pub fn note_card_path(_path: CardPath, _x: f32, _y: f32, _w: f32, _h: f32) {}
    #[inline]
    pub fn set_card_split(_on: bool) {}
    /// A release build always splits: the switch does not exist.
    #[inline]
    pub fn card_split() -> bool {
        true
    }
}

pub use imp::{
    card_split, frame_end, gate, masked, masked_part, note_card_path, note_px, set_card_split, set_clip, set_ledger,
    set_mask,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The mask parser and the ledger's class list are one table; a name that parses to no bit is
    /// the failure that reads as "this class is free".
    #[test]
    fn every_class_has_a_name_and_the_names_are_distinct() {
        assert_eq!(NAMES.len(), NCLASS);
        for (i, n) in NAMES.iter().enumerate() {
            assert!(!n.is_empty(), "class {i} has no name");
            assert_eq!(
                NAMES.iter().filter(|m| *m == n).count(),
                1,
                "duplicate class name {n}"
            );
        }
        // The enum's discriminants ARE the ledger's indices; a reorder that broke this would
        // silently attribute every card to whatever moved into slot 4.
        assert_eq!(Class::Ambient as usize, 0);
        assert_eq!(Class::Card as usize, 4);
        assert_eq!(Class::Blur as usize, 8);
        // The LAST variant closes the table: `NCLASS` is one past it, so a class added without
        // bumping `NCLASS` (or `NAMES`) fails here rather than indexing off the end of the ledger.
        assert_eq!(Class::Field as usize, NCLASS - 1);
        assert_eq!(NAMES[Class::Field as usize], "field");
    }

    /// `gfx::warm_tex` draws its offscreen quad as `Class::Blur`; that class is never refused by a
    /// mask, so a `drawmask` leg masking `image` still warms the texture. (The ledger's booking is
    /// private to the dev arm and is not read back here; `gate` returning before `add` for `Blur`
    /// is what keeps the 16x16 warm out of the panel's `image` total. No GL context on the host,
    /// so `warm_tex` itself cannot be driven from a test.)
    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_blur_class_is_never_masked_so_the_offscreen_warm_cannot_be_skipped() {
        set_ledger(true);
        set_mask("image");
        assert!(gate(Class::Image, 0.0, 0.0, 10.0, 10.0));
        assert!(!masked(Class::Blur));
        assert!(!gate(Class::Blur, 0.0, 0.0, 1280.0, 720.0));
        set_mask("");
        set_ledger(false);
    }

    /// The names the window line prints are the table the enum indexes; a path added without a name
    /// (or reordered against it) would book its draws under another path's label.
    #[test]
    fn every_card_path_has_a_distinct_name_in_enum_order() {
        assert_eq!(PATH_NAMES.len(), NPATH);
        for (i, n) in PATH_NAMES.iter().enumerate() {
            assert!(!n.is_empty() && PATH_NAMES.iter().filter(|m| *m == n).count() == 1, "path {i}: {n:?}");
        }
        assert_eq!(CardPath::Split as usize, 0);
        assert_eq!(PATH_NAMES[CardPath::Focus as usize], "focus");
        assert_eq!(PATH_NAMES[CardPath::Clipped as usize], "clip");
        assert_eq!(PATH_NAMES[CardPath::NoInterior as usize], "nointerior");
        assert_eq!(PATH_NAMES[CardPath::Plain as usize], "plain");
        assert_eq!(CardPath::Off as usize, NPATH - 1);
    }

    /// The counter's whole contract on the window line: a path's draws and its px, per frame, each
    /// path under its own name, the px the visible area of the quad (clipped to the canvas and to the
    /// live clip box, as the class ledger books it), the source-pass draws counted too, and nothing
    /// booked while the ledger is not armed. Reads the line itself, not the event log.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_window_line_books_each_card_path_once_per_draw() {
        set_ledger(false);
        note_card_path(CardPath::Split, 0.0, 0.0, 100.0, 100.0);
        assert!(imp::flush_window().contains("cardpath split=0/0.0 focus=0/0.0"), "an unarmed ledger books nothing");

        set_ledger(true);
        for _ in 0..5 {
            frame_end(); // five frames in the window (60 would log it)
        }
        for _ in 0..8 {
            note_card_path(CardPath::Split, 100.0, 100.0, 250.0, 375.0); // 93750 px each
        }
        note_card_path(CardPath::Focus, 100.0, 100.0, 250.0, 375.0);
        note_card_path(CardPath::Focus, 100.0, 100.0, 250.0, 375.0);
        note_card_path(CardPath::Clipped, -50.0, -50.0, 100.0, 100.0); // half off the canvas: 50 x 50
        note_card_path(CardPath::NoInterior, 0.0, 0.0, 10.0, 10.0);
        set_clip(Some([0.0, 0.0, 5.0, 5.0]));
        note_card_path(CardPath::Plain, 0.0, 0.0, 10.0, 10.0); // cut by the live clip: 5 x 5
        set_clip(None);
        note_card_path(CardPath::Off, 1900.0, 1060.0, 100.0, 100.0); // 20 x 20 on the canvas
        let line = imp::flush_window();
        assert!(line.starts_with("OVERDRAW frames=5 "), "{line}");
        // per frame: 8 splits of 93750 px over 5 frames = 150000 px, 1.6 draws; and so on
        assert!(
            line.ends_with("| cardpath split=150000/1.6 focus=37500/0.4 clip=500/0.2 nointerior=20/0.2 plain=5/0.2 off=80/0.2"),
            "{line}"
        );
        // the window closed: a second one starts from zero
        assert!(imp::flush_window().contains("cardpath split=0/0.0 focus=0/0.0 clip=0/0.0"));
        set_ledger(false);
    }

    /// The part names the trigger accepts are the table the enum indexes, distinct from every class
    /// name, and each is `card.` + a piece: a part added without a name, or named like a class, would
    /// mask another bit than the one its leg claims.
    #[test]
    fn every_card_part_has_a_distinct_name_in_enum_order() {
        assert_eq!(PART_NAMES.len(), NPART);
        for (i, n) in PART_NAMES.iter().enumerate() {
            assert!(n.starts_with("card.") && n.len() > "card.".len(), "part {i}: {n:?}");
            assert_eq!(PART_NAMES.iter().filter(|m| *m == n).count(), 1, "duplicate part name {n}");
            assert!(!NAMES.contains(n), "part {n:?} is also a class");
        }
        assert_eq!(CardPart::Interior as usize, 0);
        assert_eq!(PART_NAMES[CardPart::Ring as usize], "card.ring");
        assert_eq!(PART_NAMES[CardPart::Focus as usize], "card.focus");
        assert_eq!(CardPart::Focus as usize, NPART - 1);
        // and the focus part is the card-path ledger's own `focus` bucket, by name
        assert_eq!(PART_NAMES[CardPart::Focus as usize], format!("card.{}", PATH_NAMES[CardPath::Focus as usize]));
    }

    /// Each part name masks THAT part and nothing else - no class, no other part - and a class name
    /// masks no part (the card class's own gate refuses before a part is asked). The dot is part of
    /// the word: `card.ring` is not `card` followed by an unknown `ring`, which would have masked the
    /// whole class and so priced the wrong thing. Words are still separated by anything else.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn a_part_name_masks_that_part_and_nothing_else() {
        let parts = [CardPart::Interior, CardPart::Ring, CardPart::Focus];
        let classes = [
            Class::Ambient, Class::Grad, Class::Rect, Class::Shadow, Class::Card, Class::Image, Class::Text, Class::Glass,
            Class::Blur, Class::Field,
        ];
        assert_eq!(classes.len(), NCLASS, "every class, so the count below sees them all");
        let classes_masked = || classes.iter().filter(|c| masked(**c)).count();
        let parts_masked = || parts.iter().filter(|p| masked_part(**p)).copied().collect::<Vec<CardPart>>();
        assert_eq!((classes_masked(), parts_masked()), (0, vec![]), "nothing is masked until a leg says so");
        for (i, name) in PART_NAMES.iter().enumerate() {
            for spec in [name.to_string(), name.to_uppercase(), format!("  {name},"), format!("{name}\n")] {
                set_mask(&spec);
                assert_eq!((classes_masked(), parts_masked()), (0, vec![parts[i]]), "{spec:?}");
            }
        }
        set_mask("card.interior card.focus");
        assert_eq!(parts_masked(), vec![CardPart::Interior, CardPart::Focus]);
        set_mask("card.ring,text");
        assert_eq!((masked(Class::Text), masked(Class::Card), parts_masked()), (true, false, vec![CardPart::Ring]));
        // a class name masks the class and no part
        set_mask("card");
        assert_eq!((masked(Class::Card), parts_masked()), (true, vec![]));
        // `all` is every class (a part is a piece of one), and the order of the words does not matter
        set_mask("all");
        assert_eq!((classes_masked() + 1, parts_masked()), (NCLASS, vec![]), "every class but the accounting-only blur");
        set_mask("card.ring all");
        assert_eq!(parts_masked(), vec![CardPart::Ring]);
        set_mask("all card.ring");
        assert_eq!(parts_masked(), vec![CardPart::Ring]);
        // names that are not a part mask nothing: a misspelt leg must read as the control, loudly
        for bad in ["ring", "interior", "focus", "card.", "card.rim", "card.ring.focus", ".ring", "card..ring"] {
            set_mask(bad);
            assert_eq!((classes_masked(), parts_masked()), (0, vec![]), "{bad:?}");
        }
        set_mask("");
        assert_eq!((classes_masked(), parts_masked()), (0, vec![]), "disarmed");
    }

    /// The A/B switch: on by default, and off books the card as `off`, never as `plain`.
    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_card_split_switch_defaults_on_and_turns_off() {
        assert!(card_split());
        set_card_split(false);
        assert!(!card_split());
        set_card_split(true);
        assert!(card_split());
    }
}
