//! Layer and damage algebra for live backdrop sources. No GL lives here.
use crate::ui::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct Z(pub u64);
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
pub(crate) struct Layer {
    pub z: Z,
    pub rect: Rect,
    pub blocks: bool,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Damage {
    pub z: Z,
    pub rect: Rect,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Request {
    pub z: Z,
    pub rect: Rect,
    pub valid: bool,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Decision {
    pub draw: bool,
    pub refresh: bool,
}

pub(crate) fn below(layers: &[Layer], ceiling: Z) -> impl Iterator<Item = &Layer> {
    layers.iter().filter(move |layer| layer.z < ceiling)
}
pub(crate) fn decide(r: Request, layers: &[Layer], damage: &[Damage]) -> Decision {
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

pub(crate) fn intersects(a: Rect, b: Rect) -> bool {
    a.w > 0.0
        && a.h > 0.0
        && b.w > 0.0
        && b.h > 0.0
        && a.x < b.x + b.w
        && b.x < a.x + a.w
        && a.y < b.y + b.h
        && b.y < a.y + a.h
}
fn covers(a: Rect, b: Rect) -> bool {
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
pub(crate) fn canvas() -> Rect {
    Rect::new(
        0.0,
        0.0,
        crate::surface::LOGICAL_W,
        crate::surface::LOGICAL_H,
    )
}
fn union(a: Rect, b: Rect) -> Rect {
    a.union(b)
}

/// Exact draw arguments, not a probabilistic hash. Comparing the ordered commands intersecting
/// a sampler catches both motion and discrete changes without promoting chrome to page damage.
#[derive(Clone)]
struct Paint {
    z: Z,
    bounds: [u32; 4],
    values: Vec<u64>,
    glass: Option<Z>,
    source: Vec<Rc<Paint>>,
}
impl PartialEq for Paint {
    fn eq(&self, other: &Self) -> bool {
        // z selects and orders the source commands; it is not a pixel argument. A new glass
        // elsewhere can renumber an inline layer without changing any sampled pixels.
        self.bounds == other.bounds && self.values == other.values && self.source == other.source
    }
}
/// Reserved command tag shared by the painter and the source dependency walk.
pub(crate) const GLASS_COMMAND: u64 = 999;
impl Paint {
    fn rect(&self) -> Rect {
        let r = self.bounds.map(f32::from_bits);
        Rect::new(r[0], r[1], r[2], r[3])
    }
}
fn signature(
    prefix: &[Rc<Paint>],
    members: &[Rect],
    layers: &[Layer],
    ceiling: Z,
) -> Vec<Rc<Paint>> {
    prefix
        .iter()
        .filter(|p| {
            members.iter().any(|&r| {
                let r = crate::gfx::blur_region(r.x, r.y, r.w, r.h);
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

pub(crate) trait Value {
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
pub(crate) fn text_value(s: *const std::ffi::c_char, values: &mut Vec<u64>) {
    if s.is_null() {
        values.push(0);
        return;
    }
    // The same live C string the text renderer consumes; no pointer address enters identity.
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    values.push(bytes.len() as u64);
    values.extend(bytes.iter().map(|b| *b as u64));
}
pub(crate) fn clip(rect: Option<Rect>) {
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
pub(crate) fn paint(rect: Rect, values: Vec<u64>) {
    WALK.with(|w| {
        let w = w.borrow();
        let Some(w) = w.as_ref().filter(|w| w.discovery) else {
            return;
        };
        let Some(rect) = w.clip.map_or(Some(rect), |c| intersection(c, rect)) else {
            return;
        };
        if rect.w <= 0.0 || rect.h <= 0.0 {
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
        }));
    });
}

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
#[derive(Default)]
pub(crate) struct Sources {
    entries: BTreeMap<Z, Entry>,
    pub layers: Vec<Layer>,
    damage: Vec<Damage>,
    frame: u64,
    paints: Vec<std::rc::Rc<Paint>>,
    assignments: BTreeMap<(Z, u64), Z>,
}
struct Entry {
    rect: Rect,
    seen: u64,
    image: Option<Rc<crate::gfx::BackdropImage>>,
    valid: bool,
    wanted: Vec<Rect>,
    members: Vec<Rect>,
    underlay: Vec<std::rc::Rc<Paint>>,
    prefix: Vec<Rc<Paint>>,
    captured_prefix: Vec<Rc<Paint>>,
    captured_layers: Vec<Layer>,
    inline_attempt: Option<u64>,
    fell_back: bool,
}
impl Sources {
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
            }));
        }
        for (&z, e) in &mut self.entries {
            e.wanted.clear();
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
                e.valid = false;
            }
        }
        self.damage.clear();
    }
    /// Resolve CURRENT declarations before any capture. This is a geometry-only traversal of
    /// the same draw tree, not a predictor based on the previous present's set of surfaces.
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
        let mut prefixes: BTreeMap<Z, Vec<Rc<Paint>>> = BTreeMap::new();
        for (&z, e) in &mut self.entries {
            if !e.wanted.is_empty() {
                e.members = e.wanted.clone();
                if let Some(rect) = e.members.iter().copied().reduce(union) {
                    e.rect = rect;
                }
            }
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
                        p.source = signature(underlay, &[p.rect()], layers, lower);
                        Rc::new(p)
                    } else {
                        p.clone()
                    }
                })
                .collect();
            let current = signature(&prefix, &e.members, layers, z);
            let captured = signature(&e.captured_prefix, &e.members, &e.captured_layers, z);
            // Query the scene that produced the retained image at the CURRENT sampling rects.
            // Growing into an unchanged part of its captured union is not underlay damage.
            if current != captured {
                e.valid = false;
            }
            e.underlay = current;
            e.prefix = prefix;
            prefixes.insert(z, e.prefix.clone());
        }
    }
    pub fn jobs(&self) -> Vec<(Z, Rect)> {
        self.entries
            .iter()
            .filter_map(|(&z, e)| {
                let visible: Vec<_> = e
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
                    .collect();
                let refresh = !e.valid && !visible.is_empty();
                let capture = visible.into_iter().reduce(union).unwrap_or(e.rect);
                // If a lower glass contributes, capture at the visible prefix instead of replaying
                // it with a different sharp-rim source. That prefix contains its EXACT composite.
                let r = crate::gfx::blur_region(capture.x, capture.y, capture.w, capture.h);
                let footprint = Rect::new(r[0], r[1], r[2], r[3]);
                let composite = self.entries.range(..z).any(|(_, lower)| {
                    lower.members.iter().any(|r| {
                        intersects(
                            Rect::new(r.x - 4.0, r.y - 4.0, r.w + 8.0, r.h + 8.0),
                            footprint,
                        )
                    })
                });
                (e.seen == self.frame && refresh && !composite).then_some((z, capture))
            })
            .collect()
    }
    fn request(&mut self, z: Z, rect: Rect) -> Decision {
        let frame = self.frame;
        let e = self.entries.entry(z).or_insert(Entry {
            rect,
            seen: frame,
            image: None,
            valid: false,
            wanted: Vec::new(),
            members: Vec::new(),
            underlay: Vec::new(),
            prefix: Vec::new(),
            captured_prefix: Vec::new(),
            captured_layers: Vec::new(),
            inline_attempt: None,
            fell_back: false,
        });
        if !covers(e.rect, rect) {
            e.rect = union(e.rect, rect);
            if e.image.as_ref().is_none_or(|image| !image.covers(rect)) {
                e.valid = false;
            }
        }
        e.seen = frame;
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
/// RAII also covers a screen's caught panic: no source ceiling leaks into a visible pass.
pub(crate) struct Scope(Option<Walk>);
impl Drop for Scope {
    fn drop(&mut self) {
        WALK.with(|w| *w.borrow_mut() = self.0.take());
    }
}
pub(crate) fn enter(sources: Rc<RefCell<Sources>>, ceiling: Z) -> Scope {
    Scope(WALK.with(|w| {
        w.replace(Some(Walk {
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
pub(crate) fn discover(sources: Rc<RefCell<Sources>>) -> Scope {
    let scope = enter(sources, Z::ALL);
    WALK.with(|w| w.borrow_mut().as_mut().unwrap().discovery = true);
    scope
}
pub(crate) fn active() -> bool {
    WALK.with(|w| w.borrow().is_some())
}
/// A snapshot/frozen-ground boundary is part of the same walk as glass boundaries. Advance in
/// declaration and visible passes alike, so a captured ground can include arbitrary lower glass.
pub(crate) fn boundary() -> Option<Z> {
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
pub(crate) fn claim_snapshot(z: Z) -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .is_some_and(|w| w.snapshots.borrow_mut().insert(z))
    })
}

pub(crate) fn current_layer() -> Option<Z> {
    WALK.with(|w| w.borrow().as_ref().map(|w| w.current))
}
pub(crate) fn source_walk() -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .is_some_and(|w| w.discovery || w.ceiling != Z::ALL)
    })
}
pub(crate) fn discovering() -> bool {
    WALK.with(|w| w.borrow().as_ref().is_some_and(|w| w.discovery))
}
pub(crate) fn layer(z: Z, shared: bool) -> Scope {
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
pub(crate) fn draw_span<T>(name: &'static str, draw: impl FnOnce() -> T) -> T {
    if discovering() {
        draw()
    } else {
        crate::diag::spans::span(name, draw)
    }
}
pub(crate) fn suppressed() -> bool {
    WALK.with(|w| {
        w.borrow()
            .as_ref()
            .is_some_and(|w| w.stopped || w.discovery)
    })
}
#[derive(Clone, Copy)]
pub(crate) struct Surface {
    pub z: Z,
    pub rect: Rect,
    pub draw: bool,
    pub refresh: bool,
}
pub(crate) fn surface(rect: Rect) -> Option<Surface> {
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
            let r = crate::gfx::blur_region(rect.x, rect.y, rect.w, rect.h);
            let footprint = Rect::new(r[0], r[1], r[2], r[3]);
            let overlaps = w.glasses.iter().any(|&old| {
                intersects(
                    Rect::new(old.x - 4.0, old.y - 4.0, old.w + 8.0, old.h + 8.0),
                    footprint,
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
        let d = w.sources.borrow_mut().request(z, rect);
        Some(Surface {
            z,
            rect,
            draw: d.draw && !w.discovery,
            refresh: d.refresh && !w.discovery,
        })
    })
}
pub(crate) fn image(z: Z) -> Option<Rc<crate::gfx::BackdropImage>> {
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
pub(crate) fn region(z: Z) -> Option<Rect> {
    WALK.with(|w| Some(w.borrow().as_ref()?.sources.borrow().entries.get(&z)?.rect))
}
pub(crate) fn begin_inline_capture(z: Z) -> bool {
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
pub(crate) fn capture_failed(z: Z) {
    WALK.with(|w| {
        if let Some(w) = w.borrow().as_ref() {
            if let Some(e) = w.sources.borrow_mut().entries.get_mut(&z) {
                e.fell_back = true;
            }
        }
    });
}

pub(crate) fn captured(z: Z, image: crate::gfx::BackdropImage) {
    WALK.with(|w| {
        if let Some(w) = w.borrow().as_ref() {
            let mut sources = w.sources.borrow_mut();
            let layers = sources.layers.clone();
            if let Some(e) = sources.entries.get_mut(&z) {
                let recovering = e.fell_back;
                e.fell_back = false;
                let members = e.members.clone();
                e.image = Some(Rc::new(image));
                e.valid = true;
                e.captured_prefix = e.prefix.clone();
                e.captured_layers = layers;
                if recovering {
                    // Includes recovery from a failed capture whose visible fallback was flat.
                    // Overlapping upper bands have not captured yet: they use the visible prefix.
                    for (_, upper) in sources.entries.range_mut(Z(z.0 + 1)..) {
                        if upper.members.iter().any(|upper| {
                            let r = crate::gfx::blur_region(upper.x, upper.y, upper.w, upper.h);
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
        let _guard = crate::testlock::serial();
        let layers = [0, 2, 3].map(|z| Layer {
            z: Z(z),
            rect: rect(0.0),
            blocks: false,
            revision: 0,
        });
        assert_eq!(
            below(&layers, Z(2)).map(|l| l.z).collect::<Vec<_>>(),
            vec![Z(0)]
        );
    }
    #[test]
    fn a_glass_covered_by_a_frozen_or_opaque_layer_neither_refreshes_nor_draws() {
        let _guard = crate::testlock::serial();
        let layers = [Layer {
            z: Z(3),
            rect: rect(0.0),
            blocks: true,
            revision: 0,
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
        let _guard = crate::testlock::serial();
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
        let _guard = crate::testlock::serial();
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
        let _guard = crate::testlock::serial();
        let layers = [0, 2, 4].map(|z| Layer {
            z: Z(z),
            rect: rect(0.0),
            blocks: false,
            revision: 0,
        });
        assert_eq!(below(&layers, Z(2)).count(), 1);
        assert_eq!(
            below(&layers, Z(4)).map(|l| l.z).collect::<Vec<_>>(),
            vec![Z(0), Z(2)]
        );
    }
    fn declare_glass(p: crate::ui::Painter, r: Rect) {
        assert!(crate::ui::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
            p,
            r,
            0.0,
            2.0,
            [1.0; 4],
            crate::gfx::GlassRim::Standing,
            crate::gfx::GlassFace::NONE,
            crate::ui::theme::Material::UltraThin
        ));
    }
    fn commit(s: &Rc<RefCell<Sources>>) {
        let mut s = s.borrow_mut();
        let layers = s.layers.clone();
        for e in s.entries.values_mut() {
            e.valid = true;
            e.captured_prefix = e.prefix.clone();
            e.captured_layers = layers.clone();
        }
    }
    #[test]
    fn the_real_painter_ignores_foreground_and_outside_motion_but_captures_settle() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        for (i, x) in [0.0, 1.0, 2.0, 2.0].into_iter().enumerate() {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = crate::ui::Painter::root();
                p.rect(Rect::new(x, 0.0, 20.0, 20.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
                p.rect(
                    Rect::new(900.0 + i as f32, 900.0, 20.0, 20.0),
                    0.0,
                    [0.0; 4],
                    [0.0; 4],
                    0.0,
                );
                declare_glass(p, rect(0.0));
                p.rect(rect(0.0), 0.0, [i as f32; 4], [0.0; 4], 0.0);
            }
            sources.borrow_mut().resolve();
            assert_eq!(!sources.borrow().entries[&Z(1)].valid, i < 3, "frame {i}");
            commit(&sources);
        }
    }
    #[test]
    fn one_capture_per_band_covers_this_frames_union_even_on_activation() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let _band = layer(Z::CHROME, true);
            declare_glass(crate::ui::Painter::root(), rect(0.0));
            declare_glass(crate::ui::Painter::root(), rect(1000.0));
        }
        sources.borrow_mut().resolve();
        let jobs = sources.borrow().jobs();
        assert_eq!(jobs.len(), 1);
        assert!(covers(jobs[0].1, rect(0.0)) && covers(jobs[0].1, rect(1000.0)));
    }
    #[test]
    fn overlapping_bands_retain_separate_underlays_and_capture_the_lower_composite() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::ui::Painter::root();
            p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
            declare_glass(p, rect(0.0));
            declare_glass(p, rect(0.0));
        }
        sources.borrow_mut().resolve();
        let sources = sources.borrow();
        assert!(sources.entries[&Z(1)]
            .underlay
            .iter()
            .all(|p| p.glass.is_none()));
        assert!(sources.entries[&Z(2)]
            .underlay
            .iter()
            .any(|p| p.glass == Some(Z(1))));
        assert!(sources.entries[&Z(2)]
            .underlay
            .iter()
            .all(|p| p.glass != Some(Z(2))));
        assert_eq!(
            sources.jobs().iter().map(|(z, _)| *z).collect::<Vec<_>>(),
            vec![Z(1)],
            "the upper band captures in visible order, with the lower glass's exact sharp rim"
        );
    }
    #[test]
    fn the_ceiling_stops_real_primitives_and_is_restored_when_a_walk_unwinds() {
        let _guard = crate::testlock::serial();
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
        let _guard = crate::testlock::serial();
        let mut layers = [
            Layer {
                z: Z(3),
                rect: Rect::new(0.0, 0.0, 5.0, 10.0),
                blocks: true,
                revision: 0,
            },
            Layer {
                z: Z(4),
                rect: Rect::new(5.0, 0.0, 5.0, 10.0),
                blocks: true,
                revision: 0,
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
    fn adding_lower_glass_outside_the_sampled_region_does_not_refresh_shared_chrome() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        for frame in 0..2 {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = crate::ui::Painter::root();
                if frame == 1 {
                    declare_glass(p, rect(1000.0));
                }
                p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
                let _band = layer(Z::CHROME, true);
                declare_glass(p, rect(0.0));
            }
            sources.borrow_mut().resolve();
            if frame == 1 {
                assert!(sources.borrow().entries[&Z::CHROME].valid);
            }
            commit(&sources);
        }
    }

    #[test]
    fn a_moving_lower_glass_outside_the_sampled_region_does_not_refresh_it() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        for frame in 0..2 {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = crate::ui::Painter::root();
                declare_glass(p, rect(1000.0 + frame as f32));
                declare_glass(p, rect(0.0));
            }
            sources.borrow_mut().resolve();
            if frame == 1 {
                assert!(sources.borrow().entries[&Z(2)].valid);
            }
            commit(&sources);
        }
    }

    #[test]
    fn recording_painters_do_not_declare_live_glass_or_underlay_damage() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::ui::Painter::recording();
            p.rect(rect(0.0), 0.0, [0.0; 4], [0.0; 4], 0.0);
            assert!(!crate::ui::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
                p,
                rect(0.0),
                0.0,
                2.0,
                [1.0; 4],
                crate::gfx::GlassRim::Standing,
                crate::gfx::GlassFace::NONE,
                crate::ui::theme::Material::UltraThin
            ));
        }
        sources.borrow_mut().resolve();
        assert!(sources.borrow().entries.is_empty());
        assert!(sources.borrow().paints.is_empty());
    }

    #[test]
    fn a_failed_capture_cannot_retry_after_its_own_band_has_started_drawing() {
        let _guard = crate::testlock::serial();
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
    fn glass_growing_into_unchanged_captured_pixels_reuses_its_source() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        for frame in 0..2 {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = crate::ui::Painter::root();
                p.rect(rect(500.0), 0.0, [1.0; 4], [1.0; 4], 0.0);
                let _band = layer(Z::CHROME, true);
                declare_glass(
                    p,
                    Rect::new(0.0, 0.0, if frame == 0 { 10.0 } else { 700.0 }, 10.0),
                );
                declare_glass(p, rect(1000.0));
            }
            sources.borrow_mut().resolve();
            if frame == 1 {
                assert!(
                    sources.borrow().entries[&Z::CHROME].valid,
                    "unfurling chrome is not page damage"
                );
            }
            commit(&sources);
        }
    }

    #[test]
    fn a_video_plane_does_not_declare_a_framebuffer_glass_source() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        let _walk = discover(sources.clone());
        let old = crate::gfx::set_video_plane_frame(true);
        let handled = crate::ui::widgets::Glass::DYNAMIC_BACKDROP.backdrop(
            crate::ui::Painter::root(),
            rect(0.0),
            0.0,
            2.0,
            [1.0; 4],
            crate::gfx::GlassRim::Standing,
            crate::gfx::GlassFace::NONE,
            crate::ui::theme::Material::UltraThin,
        );
        crate::gfx::set_video_plane_frame(old);
        assert!(!handled);
        assert!(sources.borrow().entries.is_empty());
    }

    #[test]
    fn snapshot_claims_survive_layer_scopes_and_reset_for_each_source_walk() {
        let _guard = crate::testlock::serial();
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
    fn shared_chrome_splits_when_its_glasses_overlap() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let _band = layer(Z::CHROME, true);
            declare_glass(crate::ui::Painter::root(), rect(0.0));
            declare_glass(crate::ui::Painter::root(), rect(0.0));
            declare_glass(crate::ui::Painter::root(), rect(1000.0));
        }
        sources.borrow_mut().resolve();
        assert_eq!(
            sources.borrow().entries.keys().copied().collect::<Vec<_>>(),
            vec![Z::CHROME, Z(Z::CHROME.0 + 2), Z(Z::CHROME.0 + 3)]
        );
        assert_eq!(
            sources.borrow().jobs().len(),
            2,
            "the second band uses a visible prefix; the independent third stays above it"
        );
    }
    #[test]
    fn a_frozen_replacement_hides_lower_damage_but_its_new_image_invalidates() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        for (frame, revision) in [7, 7, 8].into_iter().enumerate() {
            sources.borrow_mut().begin(vec![Layer {
                z: Z(2),
                rect: canvas(),
                blocks: true,
                revision,
            }]);
            {
                let _walk = discover(sources.clone());
                let p = crate::ui::Painter::root();
                p.rect(rect(0.0), 0.0, [frame as f32; 4], [0.0; 4], 0.0);
                let _upper = layer(Z(3), false);
                declare_glass(p, rect(0.0));
            }
            sources.borrow_mut().resolve();
            assert_eq!(!sources.borrow().entries[&Z(4)].valid, frame != 1);
            commit(&sources);
        }
    }
    #[test]
    fn clipped_out_glass_neither_captures_nor_draws() {
        let _guard = crate::testlock::serial();
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = crate::ui::Painter::root();
            p.clip(Rect::new(0.0, 0.0, 5.0, 5.0));
            declare_glass(p, rect(100.0));
        }
        sources.borrow_mut().resolve();
        assert!(sources.borrow().entries.is_empty());
        let _walk = enter(sources, Z::ALL);
        assert!(!surface(Rect::new(-100.0, -100.0, 10.0, 10.0)).unwrap().draw);
    }

    #[test]
    fn an_asset_replaced_in_the_same_texture_name_changes_its_identity() {
        let _guard = crate::testlock::serial();
        crate::gfx::tex_ledger::specified(987654, 10, 10);
        let before = crate::gfx::tex_ledger::revision(987654);
        crate::gfx::tex_ledger::specified(987654, 10, 10);
        let after = crate::gfx::tex_ledger::revision(987654);
        crate::gfx::tex_ledger::deleted(987654);
        assert_ne!(before, after);
    }

    #[test]
    fn a_changed_draw_under_a_glass_invalidates_its_retained_source() {
        let _guard = crate::testlock::serial();
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
        let _guard = crate::testlock::serial();
        let mut sources = Sources::default();
        sources.begin(vec![]);
        sources.request(Z::CHROME, rect(0.0));
        sources.request(Z::CHROME, rect(1000.0));
        sources.resolve();
        sources.entries.get_mut(&Z::CHROME).unwrap().valid = true; // fake successful GPU capture
        sources.request(Z::OPENER, rect(500.0));
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
    fn every_changed_present_refreshes_including_the_final_settle_damage() {
        let _guard = crate::testlock::serial();
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
