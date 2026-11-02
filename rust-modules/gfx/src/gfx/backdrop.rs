//! Layer and damage algebra for live backdrop sources. No GL lives here.
//!
//! Capture jobs follow visible dependencies, not every retained entry: a lower glass hidden by a
//! frozen or opaque replacement cannot force an upper band onto the inline-framebuffer path. The
//! direct replay includes that replacement and any live dim above it, unless the glass applies
//! that dim at composite (below). The PageDip page layer (a
//! held image, or a capture frame whose source walk draws the page live) also splits stable
//! content revision from composite alpha. Its full-alpha filtered source is reused through the
//! fade; `gfx` applies the changing alpha over the constant app ground at composite
//! time, while geometry or snapshot-content revision still invalidates normally. A modal DIM
//! declared through [`paint_dim`] splits the same way: when it is the last thing drawn under a
//! glass's region, the glass filters the undimmed prefix once and `gfx` applies the dim's current
//! ink and alpha at composite ([`deferred_dim`]), so a ramping dim or a re-latched dim field
//! re-blurs nothing. The page's own WASH ([`paint_ground`]) is deferred the same way, as a DELTA: a
//! glass keeps the wash it was captured under in its source and takes `wash now - wash captured`
//! at composite ([`deferred_ground`]), so an easing wash re-blurs nothing until it settles.
//!
//! One frame draws the page in several WALKS (the discovery walk, one replay per stale source, the
//! visible walk), and [`walk_serial`] names the one that is drawing: new for every [`enter`] /
//! [`discover`], and restored with the walk when a nested walk's [`Scope`] drops. Render history
//! that is advanced once per frame and read by every walk (`ui::card_motion`) keys on it.
//!
//! This file lived at `ui/frame/backdrop.rs` and moved under `gfx` (module-layers step L5): `gfx`
//! reads and writes this walk state from its clip, clear, glass and capture paths, and the `gfx`
//! layer may not name `ui`. `ui::frame` re-exports it as `ui::frame::backdrop`, so the frame, the
//! popover host and the screens still drive it through the old path. The tests that build their
//! scene with `ui::Painter` live in `ui/frame/backdrop_tests.rs`.
// `ui/mod.rs` blankets its whole tree with this attribute, which is what kept the walk's accessors
// that only one configuration or one test calls from being dead code while this lived there.
#![allow(dead_code)]
use crate::gfx::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Z(pub u64);
impl Z {
    pub const PAGE: Self = Self(0);
    pub const CHROME: Self = Self(1 << 32);
    pub const DIM: Self = Self(2 << 32);
    pub const OPENER: Self = Self(1 << 63);
    pub const ALL: Self = Self(u64::MAX);
    pub fn page(n: usize) -> Self {
        Self((n as u64) << 24)
    }
    pub fn surface(n: usize) -> Self {
        Self((3 + n as u64) << 32)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Layer {
    pub z: Z,
    pub rect: Rect,
    pub blocks: bool,
    pub revision: u64,
    /// Composite-only opacity for an otherwise stable full-alpha source. `None` for ordinary
    /// layers; the PageDip page layer (a held image, or a capture frame whose source walk
    /// draws the page live) uses this so filtering keys on content while composition tracks the
    /// PageDip fade independently.
    pub composite_alpha: Option<f32>,
}
#[derive(Clone, Copy, Debug)]
pub struct Damage {
    pub z: Z,
    pub rect: Rect,
}
#[derive(Clone, Copy, Debug)]
pub struct Request {
    pub z: Z,
    pub rect: Rect,
    pub valid: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Decision {
    pub draw: bool,
    pub refresh: bool,
}

pub fn below(layers: &[Layer], ceiling: Z) -> impl Iterator<Item = &Layer> {
    layers.iter().filter(move |layer| layer.z < ceiling)
}
pub fn decide(r: Request, layers: &[Layer], damage: &[Damage]) -> Decision {
    let mut exposed = vec![r.rect];
    for layer in layers.iter().filter(|l| l.z > r.z && l.blocks) {
        exposed = exposed
            .into_iter()
            .flat_map(|rect| subtract(rect, layer.rect))
            .collect();
    }
    let draw = !exposed.is_empty();
    Decision {
        draw,
        refresh: draw
            && (!r.valid
                || damage
                    .iter()
                    .any(|d| d.z < r.z && intersects(d.rect, r.rect))),
    }
}

pub fn intersects(a: Rect, b: Rect) -> bool {
    a.w > 0.0
        && a.h > 0.0
        && b.w > 0.0
        && b.h > 0.0
        && a.x < b.x + b.w
        && b.x < a.x + a.w
        && a.y < b.y + b.h
        && b.y < a.y + a.h
}
pub fn covers(a: Rect, b: Rect) -> bool {
    a.x <= b.x && a.y <= b.y && a.x + a.w >= b.x + b.w && a.y + a.h >= b.y + b.h
}
fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let r = a.intersect(b);
    (r.w > 0.0 && r.h > 0.0).then_some(r)
}

fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    if !intersects(a, b) {
        return vec![a];
    }
    let (x, y, r, d) = (
        a.x.max(b.x),
        a.y.max(b.y),
        (a.x + a.w).min(b.x + b.w),
        (a.y + a.h).min(b.y + b.h),
    );
    [
        Rect::new(a.x, a.y, a.w, y - a.y),
        Rect::new(a.x, d, a.w, a.y + a.h - d),
        Rect::new(a.x, y, x - a.x, d - y),
        Rect::new(r, y, a.x + a.w - r, d - y),
    ]
    .into_iter()
    .filter(|r| r.w > 0.0 && r.h > 0.0)
    .collect()
}
pub fn canvas() -> Rect {
    Rect::new(
        0.0,
        0.0,
        plx_base::surface::LOGICAL_W,
        plx_base::surface::LOGICAL_H,
    )
}
fn union(a: Rect, b: Rect) -> Rect {
    a.union(b)
}

/// Exact draw arguments, not a probabilistic hash. Comparing the ordered commands intersecting
/// a sampler catches both motion and discrete changes without promoting chrome to page damage.
#[derive(Clone)]
pub struct Paint {
    z: Z,
    bounds: [u32; 4],
    values: Vec<u64>,
    pub glass: Option<Z>,
    source: Vec<Rc<Paint>>,
    /// This draw is a modal dim a glass above it may apply at composite instead ([`paint_dim`]).
    /// Not part of equality: `values` already carries its exact arguments.
    pub dim: Option<Dim>,
    /// This draw is the page's wash, the first thing under a glass that can show (the frame clear
    /// below it is overwritten), which that glass may take as a DELTA at composite instead of
    /// re-filtering it ([`paint_ground`]). Not part of equality.
    pub ground: Option<Ground>,
    /// With [`ground`](Self::ground): the wash's OWN springs have landed on their target (and the
    /// cascade it was drawn through is the identity), so the corners it drew are the ones it keeps
    /// ([`paint_ground`]). A control signal for [`Sources::resolve`], not a pixel argument: not
    /// part of equality, and never in a glass's signature.
    pub resting: bool,
}

/// **A modal dim, as the composite needs it**: `mix(below, ink, alpha)` where the ink is
/// `tint.rgb` times the field texture `tex` sampled at the screen point, or `tint.rgb` itself when
/// `tex` is 0 (the flat scrim ink). `tint` is the cascaded colour the draw itself would use.
///
/// The dim is LINEAR in what is under it, and its ink is a smooth field (a 60x32 reconstruction of
/// the page's light, or one colour), so `blur(mix(page, ink, a)) = mix(blur(page), ink, a)` to
/// within the blur of an already-smooth field. That is what lets a glass above it filter the
/// undimmed page once and take each frame's dim at composite. It is NOT the rejected variant that
/// multiplied the host page's RGB going into the backdrop (`ui/src/CLAUDE.md`): the composite
/// produces the same picture the in-source dim did, only without re-filtering it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dim {
    pub tex: u32,
    pub tint: [f32; 4],
}

/// The command word a dim is recorded under in a glass's signature while that glass applies it at
/// composite: its alpha, ink and field revision are the composite's inputs then, not the source's.
pub const DIM_COMMAND: u64 = 998;

/// **The page's wash, as the composite needs it**: the four corners as the GPU receives them
/// (`Painter::wash_corners` times `dim`: after the cascade's mix toward the app surface and its
/// gain, so an alpha or a gain step is part of the delta), the PLACED rect they span (not
/// [`Paint`]'s padded bounds: the bilinear is taken over this one), and the ink ramp laid over it
/// in the same pass (`ambient_inked`; zero alphas for none).
///
/// A wash is an OPAQUE bilinear field and the page's ground (drawn over the frame clear, which it
/// overwrites), so the picture under a glass is `content over wash`. While the wash eases and nothing else changes, `filter(content over wash')`
/// differs from the source the glass holds by `filter(visible * (wash' - wash))`: the glass adds
/// `wash' - wash` at composite, as if the wash showed everywhere, and the source is re-taken when the
/// wash settles ([`Ground::delta`], [`Sources::resolve`]). Bounded, not exact: see [`GroundDelta`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ground {
    /// tl, tr, br, bl, rgb 0..1.
    pub corners: [[f32; 3]; 4],
    /// `[x, y, w, h]`, authored px.
    pub rect: [f32; 4],
    /// The ink ramp's colour and its alpha at the rect's top and bottom edge.
    pub ink: [f32; 3],
    pub ink_a: [f32; 2],
}

/// The command word the wash is recorded under in a glass's signature while that glass takes it as
/// a delta: what is left of it is its geometry and ink (the source's), not its corners (the
/// composite's).
pub const GROUND_COMMAND: u64 = 997;

impl Ground {
    /// What the source keeps of this wash: its tag, rect and ink, never its corners.
    fn key(&self) -> Vec<u64> {
        let mut v = vec![GROUND_COMMAND];
        v.extend(self.rect.iter().chain(&self.ink).chain(&self.ink_a).map(|f| u64::from(f.to_bits())));
        v
    }
    /// The delta a glass whose source holds `baked` owes this frame, or `None` when it owes nothing
    /// (the same corners) or the two are not the same field (a different rect or ink: the compare
    /// has already refused that, so this is only a guard).
    pub fn delta(&self, baked: &Ground) -> Option<GroundDelta> {
        if self.corners == baked.corners || self.rect != baked.rect || self.ink != baked.ink || self.ink_a != baked.ink_a {
            return None;
        }
        Some(GroundDelta {
            corners: std::array::from_fn(|q| std::array::from_fn(|i| self.corners[q][i] - baked.corners[q][i])),
            rect: self.rect,
            weight: [1.0 - self.ink_a[0], 1.0 - self.ink_a[1]],
        })
    }
}

/// **`wash now - wash captured`, per corner**, over the wash's rect, with the ink ramp's
/// `1 - alpha` at its top and bottom edge (the wash shows through the ramp by that much). The
/// composite adds `weight(v) * bilinear(corners, u, v)` to the filtered source at the (refracted)
/// screen point `(u, v)` of the wash's own 0..1 frame, clamped.
///
/// **Bounded, not exact, and the bound is stated by what it is.** Where the wash shows, the term is
/// the exact difference (the draw itself is the bilinear field to within half a code of mesh twist,
/// `field_mesh`). Where content covers the wash, the filtered source did not contain the wash, and
/// the term adds it anyway: the error there is `|delta| * coverage` under the filter's footprint.
/// That is gone the frame the wash settles ([`Sources::resolve`] re-takes the source then), and it
/// never exceeds the ease's own amplitude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundDelta {
    pub corners: [[f32; 3]; 4],
    pub rect: [f32; 4],
    pub weight: [f32; 2],
}

impl GroundDelta {
    /// The term at authored screen point `(x, y)`: `fs_glass.frag`'s expression on the CPU. The one
    /// function the tests read the composite through.
    pub fn at(&self, x: f32, y: f32) -> [f32; 3] {
        let [rx, ry, rw, rh] = self.rect;
        let u = if rw > 0.0 { ((x - rx) / rw).clamp(0.0, 1.0) } else { 0.0 };
        let v = if rh > 0.0 { ((y - ry) / rh).clamp(0.0, 1.0) } else { 0.0 };
        let k = self.weight[0] + (self.weight[1] - self.weight[0]) * v;
        std::array::from_fn(|i| {
            let c = |q: usize| self.corners[q][i];
            let top = c(0) + (c(1) - c(0)) * u;
            let bot = c(3) + (c(2) - c(3)) * u;
            (top + (bot - top) * v) * k
        })
    }
}
impl PartialEq for Paint {
    fn eq(&self, other: &Self) -> bool {
        // z selects and orders the source commands; it is not a pixel argument. A new glass
        // elsewhere can renumber an inline layer without changing any sampled pixels.
        self.bounds == other.bounds && self.values == other.values && self.source == other.source
    }
}
/// Reserved command tag shared by the painter and the source dependency walk.
pub const GLASS_COMMAND: u64 = 999;
/// The command tag of a menu's glass on frozen ground (`gfx::draw_glass_frozen`). An ordinary
/// draw to the walk: it reads no framebuffer, so it requests no entry and owes no source.
pub const GLASS_FROZEN_COMMAND: u64 = 25;
impl Paint {
    /// The draw's kind, as a GLASSDIFF line names it (`rrect`, `shadow_outside`, …): what a frame
    /// test reads a declared list by.
    pub fn kind(&self) -> String {
        kind_name(self.values.first().copied().unwrap_or(0))
    }
    /// The draw's placed rect (logical px) from its recorded words, `[x, y, w, h]`.
    pub fn placed(&self) -> [f32; 4] {
        rect_of(self)
    }
    fn rect(&self) -> Rect {
        let r = self.bounds.map(f32::from_bits);
        Rect::new(r[0], r[1], r[2], r[3])
    }
}
fn signature(
    prefix: &[Rc<Paint>],
    members: &[Rect],
    rises: bool,
    layers: &[Layer],
    ceiling: Z,
) -> Vec<Rc<Paint>> {
    prefix
        .iter()
        .filter(|p| {
            members.iter().any(|&r| {
                let r = crate::gfx::blur_region_for(rises, r.x, r.y, r.w, r.h);
                let Some(cut) = intersection(p.rect(), Rect::new(r[0], r[1], r[2], r[3])) else {
                    return false;
                };
                let mut visible = vec![cut];
                for l in layers
                    .iter()
                    .filter(|l| l.blocks && l.z > p.z && l.z < ceiling)
                {
                    visible = visible
                        .into_iter()
                        .flat_map(|r| subtract(r, l.rect))
                        .collect();
                }
                !visible.is_empty()
            })
        })
        .cloned()
        .collect()
}

pub trait Value {
    fn record(&self, values: &mut Vec<u64>);
}
impl Value for f32 {
    fn record(&self, v: &mut Vec<u64>) {
        v.push(self.to_bits() as u64);
    }
}
impl Value for u32 {
    fn record(&self, v: &mut Vec<u64>) {
        v.push(*self as u64);
    }
}
impl Value for u64 {
    fn record(&self, v: &mut Vec<u64>) {
        v.push(*self);
    }
}
impl Value for i32 {
    fn record(&self, v: &mut Vec<u64>) {
        v.push(*self as u64);
    }
}
impl<T: Value, const N: usize> Value for [T; N] {
    fn record(&self, v: &mut Vec<u64>) {
        v.push(N as u64);
        for x in self {
            x.record(v);
        }
    }
}
impl<T: Value> Value for Option<T> {
    fn record(&self, v: &mut Vec<u64>) {
        v.push(self.is_some() as u64);
        if let Some(x) = self {
            x.record(v);
        }
    }
}
impl Value for (f32, f32) {
    fn record(&self, v: &mut Vec<u64>) {
        self.0.record(v);
        self.1.record(v);
    }
}
pub fn text_value(s: *const std::ffi::c_char, values: &mut Vec<u64>) {
    if s.is_null() {
        values.push(0);
        return;
    }
    // The same live C string the text renderer consumes; no pointer address enters identity.
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    values.push(bytes.len() as u64);
    // Eight bytes per recorded word, not one word per byte. The exact-draw-description walk
    // (this function's one job) runs on every discovered frame regardless of whether anything
    // changed — every live glass's validity depends on comparing it — so a byte-per-word
    // encoding turned every text primitive's Vec<u64> allocation, and later comparison, into one
    // word per CHARACTER: an 8x inflation on the two heaviest-text screens in the app (a
    // Settings row list, a Detail synopsis + cast bios), which is exactly where the modal-100 and
    // push-100 stress benches regressed after this mechanism replaced the special-cased fix (see
    // the dated addendum in docs/backdrop-blur-profiling.md). This is exact identity, not a
    // hash: two byte strings pack to the same words iff they are equal, with no collision risk
    // and no truncation, so `Paint::eq`'s `values == values` still means what it always meant.
    for chunk in bytes.chunks(8) {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        values.push(u64::from_le_bytes(word));
    }
}
pub fn clip(rect: Option<Rect>) {
    WALK.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            w.clip = Some(
                rect.and_then(|r| intersection(canvas(), r))
                    .unwrap_or_else(|| {
                        if rect.is_none() {
                            canvas()
                        } else {
                            Rect::new(0.0, 0.0, 0.0, 0.0)
                        }
                    }),
            );
        }
    });
}
/// True when every point of `rect` is covered by some blocking layer strictly above `z` — the
/// same subtraction `decide` already runs to tell whether a GLASS is visible, applied here to a
/// plain painted rect instead. A frozen host replacement, a `Replaced` surface's opaque ground and
/// a full-alpha dim all publish `blocks: true` (`dispatch.rs::backdrop_layers`); content under any
/// of them produces no pixels (`gfx::may_read_ground`'s `page_frozen` doc) and no live glass ever
/// samples through it — a glass ABOVE such a layer must retain that layer's own frozen composite
/// instead (`held_ceiling`'s doc), which `Sources::begin` already represents with one synthetic
/// `Paint` keyed on the layer's revision, not on what is drawn beneath it. Recording that content's
/// real primitives is therefore exactly the dead work `Z::surface(0)` already excludes above the
/// surfaces band, just bounded by a per-frame layer instead of a fixed ceiling.
fn occluded(rect: Rect, layers: &[Layer], z: Z) -> bool {
    let mut exposed = vec![rect];
    for layer in layers.iter().filter(|l| l.blocks && l.z > z) {
        exposed = exposed
            .into_iter()
            .flat_map(|r| subtract(r, layer.rect))
            .collect();
        if exposed.is_empty() {
            return true;
        }
    }
    false
}
/// Conservative, rect-free version of [`occluded`] for the fast declare-time pre-check, which
/// runs before the primitive's final padded bounds exist. Every blocking layer this mechanism has
/// ever produced covers the whole canvas, so "some blocking layer above `z` covers the canvas" is
/// exactly as precise as the exact test for all real content, and never wrongly excludes should a
/// future partial-rect blocker exist — it just misses that narrower optimization.
fn fully_occluded(z: Z, layers: &[Layer]) -> bool {
    layers.iter().any(|l| l.blocks && l.z > z && covers(l.rect, canvas()))
}
pub fn paint(rect: Rect, values: Vec<u64>) {
    paint_with(rect, values, None, None, false)
}
/// [`paint`] for a modal DIM: the same exact-argument record, plus what a glass above it needs to
/// apply it at composite instead of in its source ([`Dim`]). Whether a given glass does is decided
/// per glass in [`Sources::resolve`]: only when the dim is the last draw under its region.
pub fn paint_dim(rect: Rect, values: Vec<u64>, dim: Dim) {
    paint_with(rect, values, Some(dim), None, false)
}
/// [`paint`] for the page's WASH: the same exact-argument record, plus the corners, rect and ink a
/// glass needs to take it as a delta at composite ([`Ground`]). Whether a given glass does is decided
/// per glass in [`Sources::resolve`]: only when it is the first draw that can show under its region
/// and the only wash there.
///
/// `resting` is the wash's own at-rest signal: every spring that eases it has landed on its target
/// (`AmbientWash::at_rest`) and nothing in the cascade is moving the corners it drew. The glass
/// re-takes its source once when it becomes true ([`Sources::resolve`]); a hand-fed gradient with
/// no springs behind it passes `false` and is never re-taken for rest.
pub fn paint_ground(rect: Rect, values: Vec<u64>, ground: Ground, resting: bool) {
    paint_with(rect, values, None, Some(ground), resting)
}
fn paint_with(rect: Rect, values: Vec<u64>, dim: Option<Dim>, ground: Option<Ground>, resting: bool) {
    WALK.with(|w| {
        let w = w.borrow();
        let Some(w) = w.as_ref().filter(|w| w.discovery) else {
            return;
        };
        // Nothing at or above the surfaces band is recorded, because nothing ever reads a
        // `Paint` recorded there: a glass's source is the prefix of paints BELOW its own z, and
        // a glass in a surface (no shipped surface has one: a menu's glass stands on frozen
        // ground, `ui::popover_glass`, and requests no entry) has the page and the modal scrim
        // under the band as its prefix. Its entry is requested by `surface_with`, not here, so it
        // resolves; dropping its command word only matters to a glass stacked above it in a
        // higher surface, which would read nothing of the surface between: such a glass
        // presented over another surface (`ModalStack::present` stacks) would
        // miss that surface's changes in its cached blur — a known limit, not ruled out by
        // any gate, and the reason a second glass surface band would need recording. The surfaces
        // loop in `dispatch.rs::draw_with` (Settings/AccountMenu/ItemMenu/About's own content)
        // runs unconditionally on both the no-GL discovery pass and the real draw pass —
        // recording its entire primitive stream every discovered frame, whether or not anything
        // changed, was pure waste, and the heaviest-text screens paid for it most (see the dated
        // addendum in docs/backdrop-blur-profiling.md). This is the authoritative gate;
        // `Painter::declare`'s `recording_excluded` check exists only to skip building `values`
        // in the first place for this same content.
        if w.current >= Z::surface(0) {
            return;
        }
        let Some(rect) = w.clip.map_or(Some(rect), |c| intersection(c, rect)) else {
            return;
        };
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        // Same shape, a dynamic ceiling: a Cached host (Settings/AccountMenu over Home) still
        // walks its full page+chrome tree every discovered frame — `may_read_ground`'s doc says a
        // frozen page's draw "produces no pixels", but nothing stopped the recording underneath it
        // from happening anyway (open hypothesis, docs/backdrop-blur-profiling.md's last dated
        // addendum). `held_ceiling()`'s synthetic `Layer` already carries the frozen boundary; this
        // is the same occlusion test `signature()` already runs when building a glass's prefix, run
        // here instead so the dead content is never pushed to `paints` in the first place.
        if occluded(rect, &w.sources.borrow().layers, w.current) {
            return;
        }
        let z = w.current;
        let glass = (values.first() == Some(&GLASS_COMMAND)).then_some(z);
        w.sources.borrow_mut().paints.push(Rc::new(Paint {
            z: glass.unwrap_or(z),
            bounds: [rect.x, rect.y, rect.w, rect.h].map(f32::to_bits),
            values,
            glass,
            source: Vec::new(),
            dim,
            ground,
            resting,
        }));
    });
}
/// Fast pre-check for `ui::Painter::declare`: true once the current walk position has
/// reached the surfaces band, where [`paint`] discards everything anyway. Checking here lets a
/// caller skip building the primitive's `Vec<u64>` (and, for text, the byte-packing in
/// [`text_value`]) instead of building it only to have `paint` throw it away.
pub fn recording_excluded() -> bool {
    WALK.with(|w| {
        w.borrow().as_ref().is_some_and(|w| {
            w.discovery
                && (w.current >= Z::surface(0) || fully_occluded(w.current, &w.sources.borrow().layers))
        })
    })
}

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
/// What made two ordered draw lists unequal, for the armed `GLASSDIFF` probe. Pure: it reads the
/// same `Paint`s `Sources::resolve` compares (`Paint::eq`: bounds, exact value words, nested
/// source), so it names what `current != captured` saw without touching GL.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintDiff {
    pub prev_n: usize,
    pub now_n: usize,
    /// Index of the first draw that differs (positionally).
    pub first: usize,
    /// Number of positions that differ (a mid-list insertion shifts the tail, so this is large).
    pub changed: usize,
    pub kind: String,
    /// The draw's placed rect (logical px), from its recorded words; paint bounds for a layer.
    pub rect: [f32; 4],
    pub what: &'static str,
    /// For `what=args`: (word index, before, after) of the first differing argument word.
    pub arg: Option<(usize, u64, u64)>,
    /// Every differing position (in order), not just the first.
    pub all: Vec<DiffItem>,
}
/// One differing draw position.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffItem {
    pub index: usize,
    pub kind: String,
    pub what: &'static str,
    pub arg: Option<usize>,
    /// Old and new placed rect, for `what=rect`.
    pub rects: Option<([f32; 4], [f32; 4])>,
}
fn kind_name(tag: u64) -> String {
    match tag {
        0 => "tag0(undeclared)".into(),
        1 => "rect".into(),
        2 => "rrect".into(),
        3 => "rring".into(),
        4 => "shadow".into(),
        5 => "shadow_outside".into(),
        6 => "focus_shadow".into(),
        7 => "rect_sheened".into(),
        8 => "rect_rimmed".into(),
        9 => "rect_rimmed_w".into(),
        10 => "face_rimmed".into(),
        11 => "rrect_sheened".into(),
        12 => "tex_uv".into(),
        13 => "tex_stroked".into(),
        14 => "tex_carded".into(),
        15 => "hero_ground".into(),
        16 => "ambient".into(),
        17 => "grad4".into(),
        18 => "field".into(),
        19 => "field_ground".into(),
        20 => "field_panel".into(),
        21 => "art_scrim".into(),
        // Two painters declare tag 22 (`ambient_art`, `tex_carded_still`); the words cannot tell them apart.
        22 => "ambient_art|tex_carded_still".into(),
        23 => "ambient_inked".into(),
        100 => "text".into(),
        101 => "text_fade".into(),
        102 => "text_fade_v".into(),
        24 => "dim".into(),
        GLASS_FROZEN_COMMAND => "glass_frozen".into(),
        DIM_COMMAND => "dim(composite)".into(),
        GROUND_COMMAND => "ground(composite)".into(),
        GLASS_COMMAND => "glass".into(),
        u64::MAX => "layer".into(),
        t => format!("tag{t}"),
    }
}
// Recorded word layout of every declared draw (`ui::Painter::declare`): tag, then `[a, rgb]`
// (count word, alpha, mode), then `[x, y, w, h]` (count word, four f32 bit patterns), then the
// primitive's own arguments.
const W_ALPHA: usize = 2;
const W_MODE: usize = 3;
const W_RECT: std::ops::Range<usize> = 5..9;
const W_ARGS: usize = 9;
fn rect_of(p: &Paint) -> [f32; 4] {
    if p.values.first() != Some(&u64::MAX) && p.values.len() >= W_ARGS {
        let w = &p.values[W_RECT];
        [w[0], w[1], w[2], w[3]].map(|b| f32::from_bits(b as u32))
    } else {
        p.bounds.map(f32::from_bits)
    }
}
/// First positional difference between two draw lists, or `None` when they are equal.
pub fn diff_paints(prev: &[Rc<Paint>], now: &[Rc<Paint>]) -> Option<PaintDiff> {
    let n = prev.len().max(now.len());
    let differs = |i: usize| match (prev.get(i), now.get(i)) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    };
    let first = (0..n).find(|&i| differs(i))?;
    let changed = (first..n).filter(|&i| differs(i)).count();
    let (a, b) = (prev.get(first), now.get(first));
    let shown = b.or(a).expect("one side exists");
    let mut diff = PaintDiff {
        prev_n: prev.len(),
        now_n: now.len(),
        first,
        changed,
        kind: kind_name(shown.values.first().copied().unwrap_or(0)),
        rect: rect_of(shown),
        what: "count",
        arg: None,
        all: Vec::new(),
    };
    diff.all = (first..n)
        .filter(|&i| differs(i))
        .map(|i| item(i, prev.get(i), now.get(i)))
        .collect();
    let head = &diff.all[0];
    diff.what = head.what;
    if let (Some(index), Some(a), Some(b)) = (head.arg, a, b) {
        diff.arg = Some((index, a.values[index], b.values[index]));
    }
    Some(diff)
}
fn item(index: usize, a: Option<&Rc<Paint>>, b: Option<&Rc<Paint>>) -> DiffItem {
    let shown = b.or(a).expect("one side exists");
    let mut it = DiffItem {
        index,
        kind: kind_name(shown.values.first().copied().unwrap_or(0)),
        what: "count",
        arg: None,
        rects: None,
    };
    let (Some(a), Some(b)) = (a, b) else { return it };
    let (av, bv) = (&a.values, &b.values);
    it.what = if av.first() != bv.first() {
        "kind"
    } else if av.len() != bv.len() {
        "arglen"
    } else if av.get(W_ALPHA) != bv.get(W_ALPHA) {
        "alpha"
    } else if av.get(W_MODE) != bv.get(W_MODE) {
        "mode"
    } else if av.get(W_RECT) != bv.get(W_RECT) {
        it.rects = Some((rect_of(a), rect_of(b)));
        "rect"
    } else if let Some(i) = (W_ARGS..av.len()).find(|&i| av[i] != bv[i]) {
        it.arg = Some(i);
        "args"
    } else if av != bv {
        "values"
    } else if a.bounds != b.bounds {
        "clip"
    } else {
        "source"
    };
    it
}
/// Items listed on a line before `+N`.
const ALL_MAX: usize = 12;
fn all_field(all: &[DiffItem]) -> String {
    let r = |r: [f32; 4]| format!("{:.0},{:.0},{:.0},{:.0}", r[0], r[1], r[2], r[3]);
    let mut out = String::from("all=");
    for (n, it) in all.iter().take(ALL_MAX).enumerate() {
        if n > 0 {
            out.push('|');
        }
        out.push_str(&format!("{}:{}:{}", it.index, it.kind, it.what));
        if let Some(i) = it.arg {
            out.push_str(&format!("@{i}"));
        }
        if let Some((a, b)) = it.rects {
            out.push_str(&format!("@{}>{}", r(a), r(b)));
        }
    }
    if all.len() > ALL_MAX {
        out.push_str(&format!("|+{}", all.len() - ALL_MAX));
    }
    out
}
fn u8_of(word: u64) -> u8 {
    (f32::from_bits(word as u32).clamp(0.0, 1.0) * 255.0).round() as u8
}
/// One compact `GLASSDIFF` log line for the compare of glass `job`.
fn format_diff(frame: u64, job: Z, region: [f32; 4], prev: &[Rc<Paint>], now: &[Rc<Paint>]) -> String {
    let Some(d) = diff_paints(prev, now) else {
        return format!("GLASSDIFF frame={frame} job={:#x} what=none", job.0);
    };
    let [x, y, w, h] = d.rect;
    let arg = d.arg.map_or(String::new(), |(i, a, b)| {
        format!(
            " arg={i} was={:#x} now={:#x} f32={}>{} u8={}>{}",
            a,
            b,
            f32::from_bits(a as u32),
            f32::from_bits(b as u32),
            u8_of(a),
            u8_of(b)
        )
    });
    format!(
        "GLASSDIFF frame={frame} job={:#x} prev_n={} now_n={} first={} kind={} rect={x:.1},{y:.1},{w:.1},{h:.1} what={}{arg} changed={} region={:.0},{:.0},{:.0},{:.0} {}",
        job.0, d.prev_n, d.now_n, d.first, d.kind, d.what, d.changed,
        region[0], region[1], region[2], region[3], all_field(&d.all)
    )
}

#[derive(Default)]
pub struct Sources {
    pub entries: BTreeMap<Z, Entry>,
    pub layers: Vec<Layer>,
    damage: Vec<Damage>,
    frame: u64,
    pub paints: Vec<std::rc::Rc<Paint>>,
    assignments: BTreeMap<(Z, u64), Z>,
    /// Armed probe only: this frame's `GLASSDIFF` lines, drained by the frame loop.
    diffs: Vec<String>,
}
pub struct Entry {
    rect: Rect,
    seen: u64,
    image: Option<Rc<crate::gfx::BackdropImage>>,
    pub valid: bool,
    wanted: Vec<Rect>,
    members: Vec<Rect>,
    /// Does any member of this source RISE (slide from its rest rect)? Sets the margin its region
    /// is grown by: [`crate::gfx::blur_region_for`]. Latched from `wanted_rises` with `members`.
    rises: bool,
    wanted_rises: bool,
    pub underlay: Vec<std::rc::Rc<Paint>>,
    prefix: Vec<Rc<Paint>>,
    captured_prefix: Vec<Rc<Paint>>,
    captured_layers: Vec<Layer>,
    inline_attempt: Option<u64>,
    fell_back: bool,
    source_alpha: Option<f32>,
    /// This frame's dim, when this glass applies it at composite rather than filtering it into
    /// its source (see [`Dim`]): the dim is the LAST draw under the region, nothing above it is,
    /// and the band is not a lower glass's inline composite. Its signature then records the dim
    /// as the bare [`DIM_COMMAND`]. `None`: the dim, if any, is in the source as before.
    dim: Option<Dim>,
    /// Did the retained image leave the dim out (a direct replay that skipped it)? An image taken
    /// any other way has it baked in, and is stale the moment [`dim`](Self::dim) says otherwise.
    captured_dim: bool,
    /// This frame's wash, when this glass takes it as a DELTA at composite rather than filtering it
    /// into its source (see [`Ground`]): the wash is the first draw under the region that can show
    /// and the only one, nothing dims it, and the band replays directly. Its signature then records the wash as
    /// the bare [`GROUND_COMMAND`]. `None`: the wash, if any, is in the source as before.
    ground: Option<Ground>,
    /// The wash the retained image was drawn under: what the composite's delta is taken against.
    /// Set when an image is retained, from [`ground`](Self::ground), so a replay or an inline
    /// capture alike holds exactly the wash its frame drew.
    captured_ground: Option<Ground>,
    /// This frame's [`ground`](Self::ground) is at rest: the wash's own springs have landed (the
    /// `resting` its draw declared, [`paint_ground`]). That is the wash's statement that its ease
    /// has finished, whatever else on the page is still moving, and it is final: a landed spring
    /// is snapped onto its target, so the codes it drew cannot change again until a new ease starts.
    ground_resting: bool,
    /// Armed probe only: why the non-compare paths (`begin`'s damage, `request`'s growth) cleared
    /// `valid`, so `resolve` can name it. Never set while the probe is off.
    cause: Option<&'static str>,
}
impl Sources {
    /// The `GLASSDIFF` lines `resolve` recorded since the last call (empty unless the frame probe
    /// is armed). `mem::take` of an empty Vec allocates nothing.
    pub fn take_diffs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.diffs)
    }
    /// The dim the band at `z` applies at composite this frame — [`deferred_dim`] without a walk.
    pub fn composite_dim(&self, z: Z) -> Option<Dim> {
        self.entries.get(&z)?.dim
    }
    /// The delta the band at `z` owes at composite this frame — [`deferred_ground`] without a
    /// walk. `None` when its source holds this frame's wash (nothing is owed) or does not defer it.
    pub fn composite_ground(&self, z: Z) -> Option<GroundDelta> {
        let e = self.entries.get(&z)?;
        e.ground?.delta(&e.captured_ground?)
    }
    pub fn damage(&mut self, z: Z, rect: Rect) {
        self.damage.push(Damage { z, rect });
    }
    pub fn begin(&mut self, layers: Vec<Layer>) {
        self.frame += 1;
        self.paints.clear();
        self.assignments.clear();
        self.layers = layers;
        for layer in &self.layers {
            self.paints.push(Rc::new(Paint {
                z: layer.z,
                bounds: [layer.rect.x, layer.rect.y, layer.rect.w, layer.rect.h].map(f32::to_bits),
                values: vec![u64::MAX, layer.revision],
                glass: None,
                source: Vec::new(),
                dim: None,
                ground: None,
                resting: false,
            }));
        }
        for (&z, e) in &mut self.entries {
            e.wanted.clear();
            e.wanted_rises = false;
            if e.members.iter().any(|&rect| {
                decide(
                    Request {
                        z,
                        rect,
                        valid: e.valid,
                    },
                    &[],
                    &self.damage,
                )
                .refresh
            }) {
                if e.valid && plx_base::diag::spans::armed() {
                    e.cause = Some("damage");
                }
                e.valid = false;
            }
        }
        self.damage.clear();
    }
    /// Resolve CURRENT declarations before any capture. This is a geometry-only traversal of
    /// the same draw tree, not a predictor based on the previous present's set of surfaces.
    ///
    /// **A glass takes the page's wash at composite ([`Ground`]) when the wash is the first draw
    /// under its region that can SHOW**, and "can show" is the rule, not "was recorded first":
    /// every page opens with `gfx::frame_clear`, which a discovery walk records as a full-canvas
    /// draw of its own, so the wash is never the first record. A wash that covers the whole region
    /// and writes it opaque overwrites everything recorded before it there, so those records are
    /// invisible under it and do not make it a mid-picture layer. Anything recorded AFTER the wash
    /// (a card, text, a lower glass) can show over it, and is why the composite's term is bounded
    /// rather than exact.
    pub fn resolve(&mut self) {
        let frame = self.frame;
        let layers = &self.layers;
        self.entries.retain(|&z, e| {
            e.seen == frame
                || !decide(
                    Request {
                        z,
                        rect: e.rect,
                        valid: e.valid,
                    },
                    layers,
                    &[],
                )
                .draw
        });
        self.paints.sort_by_key(|p| p.z);
        for e in self.entries.values_mut() {
            if !e.wanted.is_empty() {
                e.members = e.wanted.clone();
                e.rises = e.wanted_rises;
                if let Some(rect) = e.members.iter().copied().reduce(union) {
                    e.rect = rect;
                }
            }
        }
        // Which bands capture INLINE (a lower glass contributes to their footprint): their image
        // is a framebuffer prefix with every dim already in it, so none of them may defer one.
        let inline: std::collections::BTreeSet<Z> = self
            .entries
            .keys()
            .copied()
            .filter(|&z| self.composite_capture(z).is_some_and(|(_, composite)| composite))
            .collect();
        let mut prefixes: BTreeMap<Z, Vec<Rc<Paint>>> = BTreeMap::new();
        let mut rises: BTreeMap<Z, bool> = BTreeMap::new();
        for (&z, e) in &mut self.entries {
            let prefix: Vec<_> = self
                .paints
                .iter()
                .filter(|p| p.z < z)
                .map(|p| {
                    if let Some((lower, underlay)) =
                        p.glass.and_then(|g| prefixes.get(&g).map(|v| (g, v)))
                    {
                        let mut p = (**p).clone();
                        // Dependencies are local to THIS lower glass, not the entire shared band.
                        // A change under its sibling must not invalidate an upper sampler here.
                        let lower_rises = rises.get(&lower).copied().unwrap_or(true);
                        p.source = signature(underlay, &[p.rect()], lower_rises, layers, lower);
                        Rc::new(p)
                    } else {
                        p.clone()
                    }
                })
                .collect();
            let full = signature(&prefix, &e.members, e.rises, layers, z);
            // A dim this glass applies at composite: the last draw under the region, the only dim
            // there, covering all of it, on a band that replays directly. Anything drawn after it
            // (an opener lift, a lower glass) would be darkened by a composite-time dim, so it
            // keeps the dim in its source instead.
            let region = e.members.iter().copied().reduce(union).map(|r| {
                let r = crate::gfx::blur_region_for(e.rises, r.x, r.y, r.w, r.h);
                Rect::new(r[0], r[1], r[2], r[3])
            });
            let deferred = full.last().filter(|last| {
                last.dim.is_some()
                    && full.iter().filter(|p| p.dim.is_some()).count() == 1
                    && !inline.contains(&z)
                    && region
                        .and_then(|r| intersection(r, canvas()))
                        .is_some_and(|r| covers(last.rect(), r))
            });
            e.dim = deferred.and_then(|p| p.dim);
            // The page's wash, taken as a delta at composite: the FIRST draw under the region that
            // can show (the opaque ground everything else is drawn over) and the only wash there,
            // covering all of it, with no dim in the source and no lower glass's composite in it.
            // "First that can show", not "first recorded": a page opens with the frame clear, a
            // full-canvas record of its own, and a wash that covers the region and writes it opaque
            // overwrites every draw recorded before it (the clear included), so those cannot show
            // and the wash is still the ground the picture under the glass is made on. Two washes
            // (a ramp knee inside the footprint, a page transition's outgoing ground) or art in the
            // ground (`ambient_art`, never marked) keep the wash in the source as before.
            let wash = full.iter().find(|p| p.ground.is_some()).filter(|first| {
                full.iter().filter(|p| p.ground.is_some()).count() == 1
                    && full.iter().all(|p| p.dim.is_none())
                    && !inline.contains(&z)
                    && region
                        .and_then(|r| intersection(r, canvas()))
                        .is_some_and(|r| covers(first.rect(), r))
            });
            e.ground = wash.and_then(|p| p.ground);
            e.ground_resting = wash.is_some_and(|p| p.resting);
            // An upper glass reads THIS one's composite through `prefixes`, and that composite
            // carries the dim at this frame's alpha and the wash it is showing, so it is handed
            // their real arguments.
            let dependents = prefix.clone();
            let swap = |p: &Rc<Paint>| {
                if deferred.is_some_and(|d| Rc::ptr_eq(p, d)) {
                    Rc::new(Paint { values: vec![DIM_COMMAND], ..(**p).clone() })
                } else if wash.is_some_and(|w| Rc::ptr_eq(p, w)) {
                    Rc::new(Paint { values: p.ground.expect("marked").key(), ..(**p).clone() })
                } else {
                    p.clone()
                }
            };
            let (prefix, current) = if deferred.is_some() || wash.is_some() {
                (prefix.iter().map(swap).collect(), full.iter().map(swap).collect())
            } else {
                (prefix, full)
            };
            let captured = signature(&e.captured_prefix, &e.members, e.rises, &e.captured_layers, z);
            // A wash that has come to rest on a code other than the one the image was drawn under:
            // re-take the source, so the picture that stays on the panel is exact. "At rest" is the
            // WASH's own statement (`ground_resting`: every spring that eases it has landed), not
            // a reading of its codes. A critically damped tail holds an 8-bit code for whole frames
            // while it still moves, so "the codes did not change since the last frame" is not
            // rest; and "nothing on the page moves" is not the wash's rest either, because a card
            // or a spinner can keep the page moving for as long as the wash has been still. A
            // landed spring is snapped onto its target, so the codes at rest are the final ones
            // and this fires once per ease.
            if e.valid && current == captured && e.ground_resting {
                if let Some(g) = e.ground {
                    if e.captured_ground != Some(g) {
                        if plx_base::diag::spans::armed() {
                            self.diffs.push(format!("GLASSDIFF frame={frame} job={:#x} what=ground-settle", z.0));
                        }
                        e.valid = false;
                    }
                }
            }
            // Query the scene that produced the retained image at the CURRENT sampling rects.
            // Growing into an unchanged part of its captured union is not underlay damage. An
            // image that has the dim (or the wash) baked in cannot serve a glass that now applies
            // it at composite, nor the reverse.
            if current != captured
                || e.dim.is_some() != e.captured_dim
                || e.ground.is_some() != e.captured_ground.is_some()
            {
                if e.valid && current == captured && plx_base::diag::spans::armed() {
                    self.diffs.push(if e.dim.is_some() != e.captured_dim {
                        format!(
                            "GLASSDIFF frame={frame} job={:#x} what=dim-mode composite={} image={}",
                            z.0, e.dim.is_some(), e.captured_dim
                        )
                    } else {
                        format!(
                            "GLASSDIFF frame={frame} job={:#x} what=ground-mode composite={} image={}",
                            z.0, e.ground.is_some(), e.captured_ground.is_some()
                        )
                    });
                }
                // Armed probe only (`plxnative-framedrop`): name the draw that made the compare
                // unequal. Unarmed this is one relaxed load and nothing else.
                if e.valid && current != captured && plx_base::diag::spans::armed() {
                    let region = e.members.iter().copied().reduce(union).map_or([0.0; 4], |r| {
                        crate::gfx::blur_region_for(e.rises, r.x, r.y, r.w, r.h)
                    });
                    self.diffs.push(format_diff(frame, z, region, &captured, &current));
                }
                e.valid = false;
            }
            if let Some(cause) = e.cause.take() {
                if plx_base::diag::spans::armed() {
                    self.diffs.push(format!("GLASSDIFF frame={frame} job={:#x} what={cause}", z.0));
                }
            }
            e.underlay = current;
            e.prefix = prefix;
            e.source_alpha = layers.iter().rev()
                .find(|layer| layer.z < z && layer.composite_alpha.is_some())
                .and_then(|layer| layer.composite_alpha);
            prefixes.insert(z, dependents);
            rises.insert(z, e.rises);
        }
    }
    pub fn jobs(&self) -> Vec<(Z, Rect)> {
        self.entries
            .iter()
            .filter_map(|(&z, e)| {
                let (capture, composite) = self.composite_capture(z)?;
                let refresh = !e.valid && capture.is_some();
                (e.seen == self.frame && refresh && !composite).then_some((z, capture.unwrap_or(e.rect)))
            })
            .collect()
    }
    /// The band at `z`'s capture rect this frame — the union of its VISIBLE members, `None` when
    /// none is — and whether a lower glass contributes to its footprint, which makes the band
    /// capture its visible prefix inline rather than replay directly. `None` for an unknown band.
    fn composite_capture(&self, z: Z) -> Option<(Option<Rect>, bool)> {
        let e = self.entries.get(&z)?;
        let visible = e
            .members
            .iter()
            .copied()
            .filter(|&rect| {
                decide(
                    Request {
                        z,
                        rect,
                        valid: e.valid,
                    },
                    &self.layers,
                    &[],
                )
                .draw
            })
            .reduce(union);
        let capture = visible.unwrap_or(e.rect);
        // If a lower glass contributes, capture at the visible prefix instead of replaying
        // it with a different sharp-rim source. That prefix contains its EXACT composite.
        let r = crate::gfx::blur_region_for(e.rises, capture.x, capture.y, capture.w, capture.h);
        let footprint = Rect::new(r[0], r[1], r[2], r[3]);
        let composite = self.entries.range(..z).any(|(&lower_z, lower)| {
            lower.members.iter().any(|r| {
                // A retained lower source hidden by a frozen/opaque replacement is not a
                // dependency of the visible prefix. Replaying to this z draws the
                // replacement (and any current dim above it), so forcing an inline
                // framebuffer capture here would merely re-capture a composite the direct
                // job already reproduces exactly.
                decide(
                    Request {
                        z: lower_z,
                        rect: *r,
                        valid: lower.valid,
                    },
                    &self.layers,
                    &[],
                ).draw && intersects(
                    Rect::new(r.x - 4.0, r.y - 4.0, r.w + 8.0, r.h + 8.0),
                    footprint,
                )
            })
        });
        Some((visible, composite))
    }
    fn request(&mut self, z: Z, rect: Rect, rises: bool) -> Decision {
        let frame = self.frame;
        let e = self.entries.entry(z).or_insert(Entry {
            rect,
            seen: frame,
            image: None,
            valid: false,
            wanted: Vec::new(),
            members: Vec::new(),
            rises: false,
            wanted_rises: false,
            underlay: Vec::new(),
            prefix: Vec::new(),
            captured_prefix: Vec::new(),
            captured_layers: Vec::new(),
            inline_attempt: None,
            fell_back: false,
            source_alpha: None,
            dim: None,
            captured_dim: false,
            ground: None,
            captured_ground: None,
            ground_resting: false,
            cause: None,
        });
        if !covers(e.rect, rect) {
            e.rect = union(e.rect, rect);
            if e.image.as_ref().is_none_or(|image| !image.covers(rect)) {
                if e.valid && plx_base::diag::spans::armed() {
                    e.cause = Some("grow");
                }
                e.valid = false;
            }
        }
        e.seen = frame;
        e.wanted_rises |= rises;
        if !e
            .wanted
            .iter()
            .any(|r| r.x == rect.x && r.y == rect.y && r.w == rect.w && r.h == rect.h)
        {
            e.wanted.push(rect);
        }
        decide(
            Request {
                z,
                rect,
                valid: e.valid,
            },
            &self.layers,
            &[],
        )
    }
    /// Drop GPU outputs while the application's GL context is still current.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.paints.clear();
    }
    pub fn resident_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|e| e.image.as_ref())
            .map(|i| i.bytes())
            .sum()
    }
    pub fn finish(&mut self) {
        // Covered sources remain discoverable while their live draws are culled by a held page.
        let layers = &self.layers;
        let frame = self.frame;

        self.entries.retain(|&z, e| {
            e.seen == frame
                || !decide(
                    Request {
                        z,
                        rect: e.rect,
                        valid: e.valid,
                    },
                    layers,
                    &[],
                )
                .draw
        });
    }
}

#[derive(Clone)]
struct Walk {
    /// Which walk this is, from [`walk_serial`]'s counter; a [`Scope`] that restores the walk
    /// restores this too.
    id: u64,
    sources: Rc<RefCell<Sources>>,
    ceiling: Z,
    layer: Z,
    next: u64,
    shared: bool,
    stopped: bool,
    discovery: bool,
    clip: Option<Rect>,
    current: Z,
    glasses: Vec<Rect>,
    snapshots: Rc<RefCell<std::collections::BTreeSet<Z>>>,
}
thread_local! { static WALK: RefCell<Option<Walk>> = const { RefCell::new(None) }; }
thread_local! { static WALKS_ENTERED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
/// **Which walk of the page is drawing**: a number that is new for every [`enter`] (and so every
/// [`discover`]) and comes back with the walk when a nested [`Scope`] drops, so it names the walk
/// and not "the latest one entered". The application draws the page several times a frame (the
/// discovery walk, a replay per stale glass source, the visible walk, and the visible walk is
/// entered FIRST and resumed after the others); a consumer that keeps per-frame history by draw
/// order (`ui::card_motion`) reads this to tell "this walk drew the card again" from "another walk
/// drew the same card". `0` while no walk is active.
pub fn walk_serial() -> u64 {
    WALK.with(|w| w.borrow().as_ref().map_or(0, |w| w.id))
}
/// RAII also covers a screen's caught panic: no source ceiling leaks into a visible pass.
pub struct Scope(Option<Walk>);
impl Drop for Scope {
    fn drop(&mut self) {
        WALK.with(|w| *w.borrow_mut() = self.0.take());
    }
}
pub fn enter(sources: Rc<RefCell<Sources>>, ceiling: Z) -> Scope {
    let id = WALKS_ENTERED.with(|n| {
        n.set(n.get().wrapping_add(1).max(1));
        n.get()
    });
    Scope(WALK.with(|w| {
        w.replace(Some(Walk {
            id,
            sources,
            ceiling,
            layer: Z::PAGE,
            next: 0,
            shared: false,
            stopped: false,
            discovery: false,
            clip: Some(canvas()),
            current: Z::PAGE,
            glasses: Vec::new(),
            snapshots: Rc::new(RefCell::new(std::collections::BTreeSet::new())),
        }))
    }))
}
pub fn discover(sources: Rc<RefCell<Sources>>) -> Scope {
    let scope = enter(sources, Z::ALL);
    WALK.with(|w| w.borrow_mut().as_mut().unwrap().discovery = true);
    scope
}
pub fn active() -> bool {
    WALK.with(|w| w.borrow().is_some())
}
/// A snapshot/frozen-ground boundary is part of the same walk as glass boundaries. Advance in
/// declaration and visible passes alike, so a captured ground can include arbitrary lower glass.
pub fn boundary() -> Option<Z> {
    WALK.with(|w| {
        let mut w = w.borrow_mut();
        let w = w.as_mut()?;
        // Reserve ordinal space on each side of a structural ground/foreground split. Adding
        // another glass inside a frozen ground must not move it ABOVE that existing snapshot.
        const SEGMENT: u64 = 1 << 16;
        w.next = (w.next / SEGMENT + 1) * SEGMENT;
        assert!(
            w.next < 1 << 24,
            "too many snapshot boundaries in one layer"
        );
        w.current = Z(w.layer.0 + w.next);
        w.stopped |= w.current >= w.ceiling;
        Some(w.current)
    })
}
pub fn claim_snapshot(z: Z) -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .is_some_and(|w| w.snapshots.borrow_mut().insert(z))
    })
}

pub fn current_layer() -> Option<Z> {
    WALK.with(|w| w.borrow().as_ref().map(|w| w.current))
}
pub fn source_walk() -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .is_some_and(|w| w.discovery || w.ceiling != Z::ALL)
    })
}
pub fn discovering() -> bool {
    WALK.with(|w| w.borrow().as_ref().is_some_and(|w| w.discovery))
}
pub fn layer(z: Z, shared: bool) -> Scope {
    let old = WALK.with(|w| {
        let old = w.borrow().clone();
        if let Some(w) = w.borrow_mut().as_mut() {
            w.layer = z;
            w.current = z;
            w.next = 0;
            w.glasses.clear();
            w.shared = shared;
            w.stopped |= z >= w.ceiling;
        }
        old
    });
    Scope(old)
}
/// One layer of the visible frame, timed as a span and selectable as a profiler phase under the
/// same name (`gfx::profile::PHASES`).
pub fn draw_span<T>(name: &'static str, draw: impl FnOnce() -> T) -> T {
    if discovering() {
        draw()
    } else {
        plx_base::diag::spans::span(name, || crate::gfx::profile::phase(name, draw))
    }
}
pub fn suppressed() -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .is_some_and(|w| w.stopped || w.discovery)
    })
}
#[derive(Clone, Copy)]
pub struct Surface {
    pub z: Z,
    pub rect: Rect,
    pub draw: bool,
    pub refresh: bool,
}
pub fn surface(rect: Rect) -> Option<Surface> {
    surface_with(rect, true)
}
/// [`surface`] for a glass that says whether it RISES (slides from its rest rect, as the hold hint
/// does). `false` sizes its region with [`BLUR_REACH`] alone — see [`crate::gfx::blur_region_for`].
pub fn surface_with(rect: Rect, rises: bool) -> Option<Surface> {
    WALK.with(|slot| {
        let mut slot = slot.borrow_mut();
        let w = slot.as_mut()?;
        w.next += 1;
        assert!(
            w.next % (1 << 16) != 0,
            "too many inline glasses between snapshot boundaries"
        );
        let key = (w.layer, w.next);
        let z = if w.discovery {
            // The DRAWN rect, not its sampling footprint, decides whether this glass leaves a
            // shared band: neighbours that merely sit within each other's `BLUR_REACH` sample
            // the page, never each other, so they keep one source and one page pass
            // ("adjacent glass samples the page", docs/liquid-glass.md). Only a glass whose
            // body overlaps an earlier one's has that glass underneath it and needs its own band.
            let overlaps = w.glasses.iter().any(|&old| {
                intersects(
                    Rect::new(old.x - 4.0, old.y - 4.0, old.w + 8.0, old.h + 8.0),
                    rect,
                )
            });
            let z = if w.shared && !overlaps && w.current == w.layer {
                w.layer
            } else {
                Z(w.layer.0 + w.next)
            };
            w.glasses.push(rect);
            w.sources.borrow_mut().assignments.insert(key, z);
            z
        } else {
            w.sources
                .borrow()
                .assignments
                .get(&key)
                .copied()
                .unwrap_or_else(|| {
                    if w.shared {
                        w.layer
                    } else {
                        Z(w.layer.0 + w.next)
                    }
                })
        };
        w.current = z;
        if w.stopped || z >= w.ceiling {
            w.stopped = true;
            return Some(Surface {
                z,
                rect,
                draw: false,
                refresh: false,
            });
        }
        let Some(rect) = w.clip.map_or(Some(rect), |c| intersection(c, rect)) else {
            return Some(Surface {
                z,
                rect,
                draw: false,
                refresh: false,
            });
        };
        if w.ceiling != Z::ALL {
            return Some(Surface {
                z,
                rect,
                draw: true,
                refresh: false,
            });
        }
        let d = w.sources.borrow_mut().request(z, rect, rises);
        Some(Surface {
            z,
            rect,
            draw: d.draw && !w.discovery,
            refresh: d.refresh && !w.discovery,
        })
    })
}
pub fn image(z: Z) -> Option<Rc<crate::gfx::BackdropImage>> {
    WALK.with(|w| {
        w.borrow()
            .as_ref()?
            .sources
            .borrow()
            .entries
            .get(&z)?
            .image
            .clone()
    })
}
/// **The current image of the band that draws a glass at exactly `rect`**, with its z — for a
/// consumer that wants the ground a glass sits on (the top bar's ground probe), asked from the
/// VISIBLE walk after the frame's source jobs have run.
///
/// "Current" is `valid` and seen this frame: an image a refused job left behind describes an older
/// page. The match is on the band's drawn members, not on coverage: a band above this glass covers
/// the same rect and holds the glass in its image. `None` in a discovery or source walk (the
/// image is not final, or is being made), with no walk, and for a rect no band lists.
pub fn member_image(rect: Rect) -> Option<(Z, Rc<crate::gfx::BackdropImage>)> {
    WALK.with(|w| {
        let w = w.borrow();
        let w = w.as_ref()?;
        if w.discovery || w.ceiling != Z::ALL {
            return None;
        }
        let sources = w.sources.borrow();
        sources.entries.iter().find_map(|(&z, e)| {
            let listed = e.members.contains(&rect);
            (listed && e.valid && e.seen == sources.frame)
                .then(|| e.image.clone().map(|image| (z, image)))
                .flatten()
        })
    })
}
/// Does the source at `z` have a rising member — the margin its region is grown by.
pub fn rises(z: Z) -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .and_then(|w| w.sources.borrow().entries.get(&z).map(|e| e.rises))
            .unwrap_or(true)
    })
}
pub fn region(z: Z) -> Option<Rect> {
    WALK.with(|w| Some(w.borrow().as_ref()?.sources.borrow().entries.get(&z)?.rect))
}
pub fn source_alpha(z: Z) -> Option<f32> {
    WALK.with(|w| Some(w.borrow().as_ref()?.sources.borrow().entries.get(&z)?.source_alpha?))
}
/// The dim the glass at `z` applies at COMPOSITE this frame (see [`Dim`]), or `None` when its
/// source has the dim in it, or there is none. `gfx` reads it only for an image whose replay left
/// the dim out.
pub fn deferred_dim(z: Z) -> Option<Dim> {
    WALK.with(|w| w.borrow().as_ref()?.sources.borrow().composite_dim(z))
}
/// **Is this the direct source replay of a band that applies its dim at composite?** Then the dim
/// is not drawn into the replay: the glass mixes it in over the filtered result instead. The
/// discovery walk and the visible walk always draw (and record) it.
pub fn dim_deferred() -> bool {
    WALK.with(|w| {
        w.borrow().as_ref().is_some_and(|w| {
            !w.discovery
                && w.ceiling != Z::ALL
                && w.sources.borrow().entries.get(&w.ceiling).is_some_and(|e| e.dim.is_some())
        })
    })
}
/// The wash delta the glass at `z` takes at COMPOSITE this frame (see [`Ground`]): this frame's
/// wash minus the one its retained image was drawn under, or `None` when they are the same, or the
/// glass keeps the wash in its source.
pub fn deferred_ground(z: Z) -> Option<GroundDelta> {
    WALK.with(|w| w.borrow().as_ref()?.sources.borrow().composite_ground(z))
}
pub fn begin_inline_capture(z: Z) -> bool {
    WALK.with(|w| {
        let w = w.borrow();
        let Some(w) = w.as_ref() else {
            return false;
        };
        let mut s = w.sources.borrow_mut();
        let frame = s.frame;
        let Some(e) = s.entries.get_mut(&z) else {
            return false;
        };
        if e.inline_attempt == Some(frame) {
            return false;
        }
        e.inline_attempt = Some(frame);
        true
    })
}
pub fn capture_failed(z: Z) {
    WALK.with(|w| {
        if let Some(w) = w.borrow().as_ref() {
            if let Some(e) = w.sources.borrow_mut().entries.get_mut(&z) {
                e.fell_back = true;
            }
        }
    });
}

pub fn captured(z: Z, image: crate::gfx::BackdropImage) {
    WALK.with(|w| {
        if let Some(w) = w.borrow().as_ref() {
            let mut sources = w.sources.borrow_mut();
            let layers = sources.layers.clone();
            if let Some(e) = sources.entries.get_mut(&z) {
                let recovering = e.fell_back;
                e.fell_back = false;
                let members = e.members.clone();
                e.captured_dim = image.dim_excluded();
                // The replay (or the inline capture) drew this frame's wash into the image.
                e.captured_ground = e.ground;
                e.image = Some(Rc::new(image));
                e.valid = true;
                e.captured_prefix = e.prefix.clone();
                e.captured_layers = layers;
                if recovering {
                    // Includes recovery from a failed capture whose visible fallback was flat.
                    // Overlapping upper bands have not captured yet: they use the visible prefix.
                    for (_, upper) in sources.entries.range_mut(Z(z.0 + 1)..) {
                        let upper_rises = upper.rises;
                        if upper.members.iter().any(|upper| {
                            let r = crate::gfx::blur_region_for(upper_rises, upper.x, upper.y, upper.w, upper.h);
                            members
                                .iter()
                                .any(|&rect| intersects(rect, Rect::new(r[0], r[1], r[2], r[3])))
                        }) {
                            upper.valid = false;
                        }
                    }
                }
            }
        }
    });
}

/// Mark every retained source as captured against the current prefix — what a successful GPU
/// capture does — so a test can step the walk frame by frame without a GL context.
#[cfg(any(test, feature = "test-support"))]
pub fn commit(s: &Rc<RefCell<Sources>>) {
    let mut s = s.borrow_mut();
    let layers = s.layers.clone();
    // Only a band that was OWED a source took one: the wash it holds is the one it was drawn
    // under, however far the wash has eased since (`Sources::resolve` tells the two apart).
    let refreshed: Vec<Z> = s.jobs().into_iter().map(|(z, _)| z).collect();
    for (z, e) in s.entries.iter_mut() {
        e.valid = true;
        // A direct replay skips the dim exactly when the band defers it.
        e.captured_dim = e.dim.is_some();
        if refreshed.contains(z) {
            e.captured_ground = e.ground;
        }
        e.captured_prefix = e.prefix.clone();
        e.captured_layers = layers.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f32) -> Rect {
        Rect::new(x, 0.0, 10.0, 10.0)
    }

    fn request() -> Request {
        Request {
            z: Z(2),
            rect: rect(0.0),
            valid: true,
        }
    }

    #[test]
    fn a_blur_source_never_contains_its_own_layer_or_above() {
        let _guard = plx_base::testlock::serial();
        let layers = [0, 2, 3].map(|z| Layer {
            z: Z(z),
            rect: rect(0.0),
            blocks: false,
            revision: 0,
            composite_alpha: None,
        });
        assert_eq!(
            below(&layers, Z(2)).map(|l| l.z).collect::<Vec<_>>(),
            vec![Z(0)]
        );
    }

    #[test]
    fn a_glass_covered_by_a_frozen_or_opaque_layer_neither_refreshes_nor_draws() {
        let _guard = plx_base::testlock::serial();
        let layers = [Layer {
            z: Z(3),
            rect: rect(0.0),
            blocks: true,
            revision: 0,
            composite_alpha: None,
        }];
        let d = decide(
            Request {
                valid: false,
                ..request()
            },
            &layers,
            &[],
        );
        assert!(!d.draw && !d.refresh);
    }

    #[test]
    fn a_glass_over_an_unchanged_region_reuses_its_source() {
        let _guard = plx_base::testlock::serial();
        let d = decide(
            request(),
            &[],
            &[Damage {
                z: Z(2),
                rect: rect(0.0),
            }],
        );
        assert!(
            d.draw && !d.refresh,
            "foreground motion is not underlay damage"
        );
    }

    #[test]
    fn damage_outside_a_glass_rect_does_not_refresh_it() {
        let _guard = plx_base::testlock::serial();
        assert!(
            !decide(
                request(),
                &[],
                &[Damage {
                    z: Z(0),
                    rect: rect(20.0)
                }]
            )
            .refresh
        );
    }

    #[test]
    fn two_overlapping_glasses_at_different_z_each_sample_only_what_is_below_them() {
        let _guard = plx_base::testlock::serial();
        let layers = [0, 2, 4].map(|z| Layer {
            z: Z(z),
            rect: rect(0.0),
            blocks: false,
            revision: 0,
            composite_alpha: None,
        });
        assert_eq!(below(&layers, Z(2)).count(), 1);
        assert_eq!(
            below(&layers, Z(4)).map(|l| l.z).collect::<Vec<_>>(),
            vec![Z(0), Z(2)]
        );
    }

    /// **A glass finds ITS band's current image by its rect, and no other.** Two bands whose
    /// images both cover the canvas: the answer for each rect is its own band's (a band above would
    /// hold the glass in its image), nothing answers for a rect no band draws, a stale image
    /// (`valid` false) answers nothing, and neither a discovery nor a source walk answers at all.
    #[test]
    fn a_glass_finds_its_own_bands_current_image_by_rect_and_no_other() {
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        let (a, b) = (Rect::new(100.0, 40.0, 300.0, 76.0), Rect::new(900.0, 40.0, 300.0, 76.0));
        let (za, zb);
        {
            let _walk = discover(sources.clone());
            assert!(member_image(a).is_none(), "a discovery walk has no final image");
            za = surface(a).unwrap().z;
            zb = surface(b).unwrap().z;
        }
        assert_ne!(za, zb, "an unshared layer gives each glass its own band");
        sources.borrow_mut().resolve();
        let whole = [0.0, 0.0, 1920.0, 1080.0];
        for (z, e) in sources.borrow_mut().entries.iter_mut() {
            let mw = if *z == za { 10 } else { 20 };
            e.image = Some(Rc::new(crate::gfx::BackdropImage::for_test(whole, mw, 8)));
            e.valid = true;
        }
        {
            let _walk = enter(sources.clone(), Z::ALL);
            let (z, image) = member_image(a).expect("a's band has a current image");
            assert_eq!((z, image.bytes()), (za, 10 * 8 * 4));
            let (z, image) = member_image(b).expect("b's band has a current image");
            assert_eq!((z, image.bytes()), (zb, 20 * 8 * 4));
            assert!(member_image(Rect::new(500.0, 40.0, 300.0, 76.0)).is_none(), "no band draws this rect");
        }
        sources.borrow_mut().entries.get_mut(&za).unwrap().valid = false;
        let _walk = enter(sources.clone(), Z::ALL);
        assert!(member_image(a).is_none(), "an image a refused job left behind is not the ground");
        assert!(member_image(b).is_some());
        let _source = enter(sources.clone(), Z(5));
        assert!(member_image(b).is_none(), "a source walk is drawing the image, not reading it");
    }

    #[test]
    fn the_ceiling_stops_real_primitives_and_is_restored_when_a_walk_unwinds() {
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        let child_stayed_below = std::cell::Cell::new(false);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _walk = enter(sources, Z(2));
            assert!(!crate::gfx::culled(0.0, 0.0, 10.0, 10.0));
            assert!(surface(rect(0.0)).unwrap().draw);
            assert!(!surface(rect(0.0)).unwrap().draw);
            assert!(crate::gfx::culled(0.0, 0.0, 10.0, 10.0));
            {
                let _cached_child = layer(Z::PAGE, false);
                child_stayed_below.set(crate::gfx::culled(0.0, 0.0, 10.0, 10.0));
            }
            panic!("exercise source-scope unwinding");
        }));
        assert!(caught.is_err());
        assert!(
            child_stayed_below.get(),
            "a nested cached layer cannot reopen the source prefix"
        );
        assert!(!suppressed());
    }

    #[test]
    fn multiple_partial_blockers_cover_a_glass_but_translucent_layers_do_not() {
        let _guard = plx_base::testlock::serial();
        let mut layers = [
            Layer {
                z: Z(3),
                rect: Rect::new(0.0, 0.0, 5.0, 10.0),
                blocks: true,
                revision: 0,
                composite_alpha: None,
            },
            Layer {
                z: Z(4),
                rect: Rect::new(5.0, 0.0, 5.0, 10.0),
                blocks: true,
                revision: 0,
                composite_alpha: None,
            },
        ];
        assert!(!decide(request(), &layers, &[]).draw);
        layers[1].blocks = false;
        assert!(decide(request(), &layers, &[]).draw);
        layers[1].blocks = true;
        layers[1].z = Z(0);
        assert!(decide(request(), &layers, &[]).draw);
    }

    #[test]
    fn recording_excluded_follows_the_frozen_host_boundary_too() {
        // `Painter::declare`'s fast pre-check must skip building a primitive's `Vec<u64>` (and, for
        // text, the byte-packing) under a frozen host boundary the same way it already does above
        // the surfaces band, so the heaviest-text screens under Home (a Settings row list is drawn
        // in the surface, but Home's own shelves/hero/cast rows sit BELOW the frozen boundary and
        // still walk) do not pay for building data `paint` would just discard.
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![Layer {
            z: Z(5),
            rect: canvas(),
            blocks: true,
            revision: 1,
            composite_alpha: None,
        }]);
        let _walk = discover(sources.clone());
        {
            let _layer = layer(Z(1), false);
            assert!(
                recording_excluded(),
                "below the frozen boundary, declare's pre-check should skip"
            );
        }
        {
            let _layer = layer(Z(6), false);
            assert!(
                !recording_excluded(),
                "above the frozen boundary, content is still live and must still record"
            );
        }
    }

    #[test]
    fn a_failed_capture_cannot_retry_after_its_own_band_has_started_drawing() {
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        let _walk = enter(sources.clone(), Z::ALL);
        let _band = layer(Z::CHROME, true);
        let first = surface(rect(0.0)).unwrap();
        assert!(begin_inline_capture(first.z));
        let second = surface(rect(1000.0)).unwrap();
        assert!(
            !begin_inline_capture(second.z),
            "a retry would sample the first owner's fallback"
        );
        sources.borrow_mut().begin(vec![]);
        assert!(
            begin_inline_capture(first.z),
            "retry on the next frame's clean prefix"
        );
    }

    #[test]
    fn a_walk_keeps_its_serial_through_nested_walks_and_layers_and_every_walk_has_its_own() {
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        assert_eq!(walk_serial(), 0, "no walk, no serial");
        let visible = enter(sources.clone(), Z::ALL);
        let visible_serial = walk_serial();
        assert_ne!(visible_serial, 0);
        {
            let _layer = layer(Z::surface(0), false);
            assert_eq!(walk_serial(), visible_serial, "a layer is part of its walk");
        }
        let discovery_serial = {
            let _discovery = discover(sources.clone());
            walk_serial()
        };
        assert_eq!(walk_serial(), visible_serial, "the walk that was entered first is back");
        let replay_serial = {
            let _replay = enter(sources.clone(), Z::CHROME);
            walk_serial()
        };
        assert_eq!(walk_serial(), visible_serial, "and again after a source replay");
        assert!(
            visible_serial != discovery_serial
                && visible_serial != replay_serial
                && discovery_serial != replay_serial,
            "each walk is its own: {visible_serial} {discovery_serial} {replay_serial}"
        );
        drop(visible);
        assert_eq!(walk_serial(), 0);
    }

    #[test]
    fn snapshot_claims_survive_layer_scopes_and_reset_for_each_source_walk() {
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        {
            let _walk = enter(sources.clone(), Z::OPENER);
            assert!(claim_snapshot(Z::DIM));
            let _layer = layer(Z::surface(0), false);
            assert!(!claim_snapshot(Z::DIM));
        }
        let _walk = enter(sources, Z::OPENER);
        assert!(claim_snapshot(Z::DIM));
    }

    #[test]
    fn an_asset_replaced_in_the_same_texture_name_changes_its_identity() {
        let _guard = plx_base::testlock::serial();
        crate::gfx::tex_ledger::specified(987654, 10, 10);
        let before = crate::gfx::tex_ledger::revision(987654);
        crate::gfx::tex_ledger::specified(987654, 10, 10);
        let after = crate::gfx::tex_ledger::revision(987654);
        crate::gfx::tex_ledger::deleted(987654);
        assert_ne!(before, after);
    }

    #[test]
    fn a_changed_draw_under_a_glass_invalidates_its_retained_source() {
        let _guard = plx_base::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        for (frame, color) in [1u64, 2].into_iter().enumerate() {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                paint(rect(0.0), vec![color]);
                surface(rect(0.0));
            }
            sources.borrow_mut().resolve();
            if frame == 1 {
                assert!(
                    !sources.borrow().entries[&Z(1)].valid,
                    "changed paint is damage without a page-motion verdict"
                );
            }
            commit(&sources);
        }
    }

    #[test]
    fn damage_in_the_gap_between_shared_glasses_does_not_refresh_the_band() {
        let _guard = plx_base::testlock::serial();
        let mut sources = Sources::default();
        sources.begin(vec![]);
        sources.request(Z::CHROME, rect(0.0), true);
        sources.request(Z::CHROME, rect(1000.0), true);
        sources.resolve();
        sources.entries.get_mut(&Z::CHROME).unwrap().valid = true; // fake successful GPU capture
        sources.request(Z::OPENER, rect(500.0), true);
        sources.resolve();
        assert!(
            sources.jobs().iter().any(|(z, _)| *z == Z::OPENER),
            "a capture union's gap is not a lower glass dependency"
        );
        sources.damage(Z::PAGE, rect(500.0));
        sources.begin(vec![]);
        assert!(
            sources.entries[&Z::CHROME].valid,
            "the union's empty gap is not a sampler"
        );
    }

    #[test]
    fn text_value_packs_bytes_instead_of_one_word_per_byte() {
        let _guard = plx_base::testlock::serial();
        // Long enough that a byte-per-word encoding would visibly balloon: this is exactly the
        // shape a Settings row title or a Detail synopsis produces every discovered frame.
        let s = std::ffi::CString::new(
            "a string long enough to prove packing happened, not truncation or hashing",
        )
        .unwrap();
        let byte_len = s.as_bytes().len();
        let mut values = Vec::new();
        text_value(s.as_ptr(), &mut values);
        // One length-prefix word plus one packed word per (up to) 8 bytes — never one word per
        // byte, which is what made every text primitive's recorded Vec<u64> as long as the
        // string itself, reallocated and re-hashed/-compared on every single discovery walk.
        assert_eq!(
            values.len(),
            1 + (byte_len + 7) / 8,
            "text_value must pack 8 bytes per recorded word, not one byte per word"
        );
        // Exact identity, not a hash: the same string encodes identically every time, and a
        // different string (even one differing only after the first packed word) encodes
        // differently — the walk's whole basis for "changed vs unchanged" depends on this.
        let mut again = Vec::new();
        text_value(s.as_ptr(), &mut again);
        assert_eq!(values, again);
        let other = std::ffi::CString::new(
            "a string long enough to prove packing happened, NOT truncation or hashing",
        )
        .unwrap();
        let mut other_values = Vec::new();
        text_value(other.as_ptr(), &mut other_values);
        assert_ne!(values, other_values);
    }

    #[test]
    fn every_changed_present_refreshes_including_the_final_settle_damage() {
        let _guard = plx_base::testlock::serial();
        for _ in 0..10 {
            assert!(
                decide(
                    request(),
                    &[],
                    &[Damage {
                        z: Z(0),
                        rect: rect(0.0)
                    }]
                )
                .refresh
            );
        }
        assert!(!decide(request(), &[], &[]).refresh);
    }
}

#[cfg(test)]
mod glassdiff_tests {
    use super::*;
    fn draw(x: f32, a: f32) -> Rc<Paint> {
        let mut v = vec![1u64];
        [a, 0.0f32].record(&mut v);
        [x, 10.0, 100.0, 50.0].record(&mut v);
        1.0f32.record(&mut v);
        Rc::new(Paint {
            z: Z(0),
            bounds: [x, 10.0, 100.0, 50.0].map(f32::to_bits),
            values: v,
            glass: None,
            source: Vec::new(),
            dim: None,
            ground: None,
            resting: false,
        })
    }
    #[test]
    fn a_moved_rect_is_named() {
        let prev = [draw(0.0, 1.0), draw(5.0, 1.0)];
        let now = [draw(0.0, 1.0), draw(9.0, 1.0)];
        let d = diff_paints(&prev, &now).unwrap();
        assert_eq!((d.first, d.changed, d.what, d.kind.as_str()), (1, 1, "rect", "rect"));
        assert_eq!(d.rect, [9.0, 10.0, 100.0, 50.0]);
        let line = format_diff(7, Z::CHROME, [0.0, 0.0, 300.0, 100.0], &prev, &now);
        assert!(line.contains("what=rect") && line.contains("kind=rect"), "{line}");
        assert!(line.contains("region=0,0,300,100"), "{line}");
        assert!(line.ends_with("all=1:rect:rect@5,10,100,50>9,10,100,50"), "{line}");
    }
    #[test]
    fn all_differing_draws_are_listed_in_order_and_truncate_with_a_count() {
        let prev: Vec<_> = (0..16).map(|i| draw(i as f32, 1.0)).collect();
        let mut now: Vec<_> = prev.clone();
        now[1] = draw(100.0, 1.0);
        now[3] = draw(3.0, 0.5);
        let d = diff_paints(&prev, &now).unwrap();
        assert_eq!(d.changed, 2);
        let f = all_field(&d.all);
        assert_eq!(f, "all=1:rect:rect@1,10,100,50>100,10,100,50|3:rect:alpha");
        let now: Vec<_> = (0..16).map(|i| draw(i as f32 + 50.0, 1.0)).collect();
        let f = all_field(&diff_paints(&prev, &now).unwrap().all);
        assert_eq!(f.matches(":rect:rect@").count(), 12);
        assert!(f.starts_with("all=0:rect:rect@") && f.ends_with("|+4"), "{f}");
    }
    #[test]
    fn identical_frames_produce_no_diff() {
        let a = [draw(0.0, 1.0), draw(5.0, 0.5)];
        let b = [draw(0.0, 1.0), draw(5.0, 0.5)];
        assert!(diff_paints(&a, &b).is_none());
    }
    #[test]
    fn alpha_and_count_are_told_apart() {
        let d = diff_paints(&[draw(0.0, 1.0)], &[draw(0.0, 0.5)]).unwrap();
        assert_eq!(d.what, "alpha");
        let d = diff_paints(&[draw(0.0, 1.0)], &[draw(0.0, 1.0), draw(1.0, 1.0)]).unwrap();
        assert_eq!((d.what, d.first, d.now_n), ("count", 1, 2));
    }
    #[test]
    fn a_colour_word_reports_8_bit_values() {
        let (mut a, mut b) = (draw(0.0, 1.0), draw(0.0, 1.0));
        let last = a.values.len() - 1;
        Rc::get_mut(&mut a).unwrap().values[last] = 0.0f32.to_bits() as u64;
        Rc::get_mut(&mut b).unwrap().values[last] = 1.0f32.to_bits() as u64;
        let d = diff_paints(&[a], &[b]).unwrap();
        assert_eq!(d.what, "args");
        let (_, was, now) = d.arg.unwrap();
        assert_eq!((u8_of(was), u8_of(now)), (0, 255));
    }
    #[test]
    fn unarmed_resolve_invalidates_but_records_nothing() {
        let _guard = plx_base::testlock::serial();
        assert!(!plx_base::diag::spans::armed());
        let mut s = Sources::default();
        s.begin(Vec::new());
        let member = Rect::new(0.0, 10.0, 100.0, 50.0);
        s.request(Z::CHROME, member, true);
        s.paints.push(draw(5.0, 1.0));
        let e = s.entries.get_mut(&Z::CHROME).unwrap();
        e.valid = true;
        e.captured_prefix = vec![draw(0.0, 1.0)];
        s.resolve();
        assert!(!s.entries[&Z::CHROME].valid, "the compare still invalidates");
        assert!(s.take_diffs().is_empty());
    }
}
