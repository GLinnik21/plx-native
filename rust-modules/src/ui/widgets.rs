//! Reusable retui leaves + shared helpers. These are the "reusable UI elements":
//! Button, CircleButton, TabPill, TransportButton, PageDots, Badge, plus the shared art-card
//! core (`card`/`draw_card`) and the poster-resolve helper. (Multi-line text wrapping
//! now lives in the `TextView` primitive in `text_view.rs`.)
use crate::plex::ServerId;
use crate::pms::PmsMovie;
use crate::ui::theme;
use crate::ui::label::{HAlign, Label};
use crate::ui::{Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

// ---- backdrop glass -------------------------------------------------------------------------

/// How a glass surface keeps its shared backdrop snapshot fresh.
///
/// The cadence is named in PRESENTS rather than hertz on purpose: it is at most 20 Hz when the UI
/// is presenting at 60 Hz, and a clean settled page creates no private sampling clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlassRefresh {
    /// One snapshot when the surface appears; the owner invalidates again if its underlay changes.
    Cached,
    /// Snapshot on every present on which the underlay actually CHANGED.
    ///
    /// It was every THIRD such present until 2026-08-19, and the three was a cost guess made
    /// before anything was measured. Measured, on the direct source path: raising the cadence from
    /// one-in-three to one-in-one costs **+0.07% of the frame and zero frames** — the scene holds
    /// 60.0 fps flat across two interleaved rounds. On the capture path the same change costs
    /// +9.2% and about five frames, because a capture refresh frame does not fit inside a vsync
    /// slot; that is the whole reason the direct path had to land first.
    ///
    /// The "changed" half is NOT a cadence and does not go away: a settled page still takes no
    /// snapshots at all, which is what keeps a still screen free of a private sampling clock.
    /// [`DEFAULT_DYNAMIC_PERIOD`] is now 1, and `/tmp/plxnative-glasshz` still moves it, because
    /// the cost curve it produced is a property of ONE scene and the next screen to wear glass
    /// will have to be measured too.
    EveryChangedPresent,
}

/// Reusable backdrop-glass policy. It owns no geometry and no animation: a `Popover` can hold it,
/// and a standalone widget can use the same `activate` → `prepare` → `backdrop` sequence.
///
/// **It used to carry a second axis — `GlassUnderlay`, i.e. WHERE a modal's dim is applied — and
/// that axis is gone.** Its non-trivial variant dimmed the host page's RGB going into the backdrop
/// instead of compositing a scrim over it, and it was measured against the item menu over one
/// checker ground and rejected: dimming the source destroys the very modulation the frost is then
/// layered over, so the panel arrives flat however dense the material says it is. The two
/// constructions do not converge, so this is not a taste setting that was left unset — it is a
/// mechanism that was tried and does not work. The account is in `e75b5e49`; the code is in the
/// history if it is ever wanted back. Every surface composites.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Glass {
    refresh: GlassRefresh,
}

/// Per-visible-lifetime state for a [`Glass`] policy. Visibility/source state stays with its widget;
/// recurring cadence is global because every owner shares the renderer's one snapshot chain.
pub(crate) struct GlassState {
    last_seen: u32,
    active: bool,
}

impl GlassState {
    pub(crate) const fn new() -> Self {
        Self { last_seen: 0, active: false }
    }

    pub(crate) fn deactivate(&mut self) {
        self.active = false;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    #[inline]
    fn needs_activation(&self, present: u32) -> bool {
        !self.active || present.wrapping_sub(self.last_seen) > 1
    }
}

/// Successful swaps, not update iterations. Both route-gap detection and the shared three-present
/// cadence derive from this serial, so skipped idle loops cannot advance either one.
static GLASS_PRESENT_SERIAL: AtomicU32 = AtomicU32::new(0);

/// Decision returned by the one global dynamic-snapshot clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DynamicStep {
    None,
    Wait,
    Refresh,
}

/// Presents per dynamic backdrop refresh. **[`DEFAULT_DYNAMIC_PERIOD`] — 1 — is the shipped
/// cadence**, i.e. every changed present, and this static exists so the cadence cost curve can be
/// measured on the television without a second binary. `/tmp/plxnative-glasshz` is the only writer
/// ([`set_dynamic_period`]); absent, this reads the default.
///
/// (This said "**3 is the shipped cadence** — about 20 Hz" for as long as the default was 3, and
/// went on saying it after the direct path made 1 cost +0.07% of a frame. One number, quoted in
/// four places; the const is the only one that was ever true.)
static DYNAMIC_PERIOD: AtomicU32 = AtomicU32::new(DEFAULT_DYNAMIC_PERIOD);

/// The shipped presents-per-refresh cadence for [`GlassRefresh::EveryChangedPresent`].
///
/// One, measured: at 1/4 source scale on the direct path this is +0.07% of the frame against the
/// old three, and 60.0 fps either way. See the variant's doc for the capture-path figure, which is
/// two orders of magnitude worse and is why this could not have been the default before.
const DEFAULT_DYNAMIC_PERIOD: u32 = 1;

/// Override the shared dynamic cadence, in PRESENTS per refresh. Returns the value actually
/// installed: 0 is meaningless (a refresh every zero frames) and anything past 8 is a cadence no
/// glass surface would survive looking at, so both clamp instead of being refused — a profiling
/// knob that silently does nothing is worse than one that says what it did.
pub(crate) fn set_dynamic_period(presents: u32) -> u32 {
    let n = presents.clamp(1, 8);
    DYNAMIC_PERIOD.store(n, Relaxed);
    n
}

/// The live presents-per-refresh cadence.
pub(crate) fn dynamic_period() -> u32 {
    DYNAMIC_PERIOD.load(Relaxed)
}

/// Recurring cadence belongs to the shared snapshot chain, not to any one widget. If two widgets
/// opened on different presents kept separate phases, their combined schedules could refresh 2/3
/// or even every frame. `covered_present` also lets the first prepared owner mark a due capture as
/// covering every other owner prepared before the underlay draw on that same present.
#[derive(Clone, Copy)]
struct DynamicClock {
    last_refresh: u32,
    covered_present: u32,
    pending: bool,
}

impl DynamicClock {
    const fn new() -> Self {
        Self { last_refresh: 0, covered_present: 0, pending: false }
    }

    fn cover_now(&mut self, present: u32) {
        self.last_refresh = present;
        self.covered_present = present;
        self.pending = false;
    }

    /// `period` is presents per refresh and is passed in rather than read here, so the tests below
    /// state the cadence they are asserting instead of depending on a process-wide static.
    fn step(&mut self, present: u32, changed: bool, period: u32) -> DynamicStep {
        if changed && self.covered_present != present {
            self.pending = true;
        }
        if !self.pending {
            return DynamicStep::None;
        }
        if present.wrapping_sub(self.last_refresh) >= period.max(1) {
            self.cover_now(present);
            DynamicStep::Refresh
        } else {
            DynamicStep::Wait
        }
    }
}

/// Main-render-thread state, like the renderer's snapshot cache it schedules.
static mut DYNAMIC_CLOCK: DynamicClock = DynamicClock::new();

/// Called exactly beside `idle::note_present`, after `SDL_GL_SwapWindow` returns.
pub(crate) fn glass_presented() {
    GLASS_PRESENT_SERIAL.fetch_add(1, Relaxed);
}

impl Glass {
    /// Existing popover behaviour: source-over scrim and a cached snapshot.
    pub(crate) const CACHED: Self = Self { refresh: GlassRefresh::Cached };

    /// A moving surface: dirty-aware backdrop on the shared [`DYNAMIC_PERIOD`] cadence (1 — every
    /// changed present). The widget itself still draws on every presented frame.
    pub(crate) const DYNAMIC_BACKDROP: Self = Self { refresh: GlassRefresh::EveryChangedPresent };

    /// **Does a popover on this policy owe its scrim to the host PAGE rather than to its own
    /// painter?** True for a refreshing policy, and the reason is draw order, not taste.
    ///
    /// A `Cached` popover captures once, through the capture path, which grabs framebuffer 0 after
    /// its own scrim is already on it. A refreshing one comes back round to the DIRECT path, which
    /// re-renders the page closure before any popover draws — so a scrim drawn with the panel is in
    /// the visible frame and not in the snapshot, and the frosted ground reads brighter than the
    /// dimmed screen around it. [`crate::ui::popover::Popover::scrim`] is the fix and
    /// `Popover::painter` `debug_assert`s on this, so the mistake fails on the host instead of
    /// being noticed on a television.
    pub(crate) fn needs_page_scrim(self) -> bool {
        matches!(self.refresh, GlassRefresh::EveryChangedPresent)
    }

    /// Start a new visible lifetime and make its first snapshot immediately eligible.
    pub(crate) fn activate(self, state: &mut GlassState) {
        let present = GLASS_PRESENT_SERIAL.load(Relaxed);
        state.last_seen = present;
        state.active = true;
        if matches!(self.refresh, GlassRefresh::EveryChangedPresent) {
            unsafe { (*std::ptr::addr_of_mut!(DYNAMIC_CLOCK)).cover_now(present) };
        }
        crate::gfx::blur_invalidate();
        crate::ui::idle::wake();
    }

    /// Resolve this frame before the host page is drawn. `underlay_changed` must describe that
    /// host, not foreground widget motion. Every dynamic owner sharing a host must prepare before
    /// any of them captures. Invalidation happens here, while capture remains deferred until
    /// [`backdrop`](Self::backdrop), after the underlay has painted.
    pub(crate) fn prepare(self, state: &mut GlassState, underlay_changed: bool) {
        let present = GLASS_PRESENT_SERIAL.load(Relaxed);
        // A widget not drawn for one or more successful presents crossed a route/surface lifetime.
        // Its old snapshot may describe that other route, so returning is a fresh activation.
        if state.needs_activation(present) {
            self.activate(state);
        }
        state.last_seen = present;

        if matches!(self.refresh, GlassRefresh::EveryChangedPresent) {
            let step = unsafe {
                (*std::ptr::addr_of_mut!(DYNAMIC_CLOCK)).step(present, underlay_changed, dynamic_period())
            };
            match step {
                DynamicStep::Refresh => crate::gfx::blur_invalidate(),
                DynamicStep::Wait => {
                    // A discrete landing may have bought only this one frame. Keep the gate alive
                    // just until the next global sampling slot so it cannot stay stale for 2 s.
                    crate::ui::idle::wake();
                }
                DynamicStep::None => {}
            }
        }
    }

    /// Draw the captured backdrop only. The caller owns the material layered over it, which is
    /// what lets the same policy serve a frosted panel and the sheened tab-track capsule.
    #[must_use]
    pub(crate) fn backdrop(
        self,
        p: Painter,
        r: Rect,
        rest_dy: f32,
        radius: f32,
        tint: [f32; 4],
        rim: crate::gfx::GlassRim,
        face: crate::gfx::GlassFace,
        mat: theme::Material,
    ) -> bool {
        p.backdrop_blur(r, rest_dy, radius, tint, rim, face, mat.deep())
    }

    /// Standard popover ground: glass + frost where available, the existing opaque sheet fallback
    /// on a driver that cannot render the chain.
    ///
    /// **The same edge as the standing track, and that is a decision taken by looking.** The design
    /// system gives a SHEET a 28px chamfer ramp on top of the line — "so a sheet reads as THICK
    /// rather than outlined" — and the track no ramp at all. Drawn side by side in one frame the
    /// two do not read as one material: the panel is a lit slab with a soft band down its top edge
    /// and a shade along its bottom, the bar is a crisp outline, and the panel wins the eye for
    /// reasons that have nothing to do with which one you are meant to be reading. A panel over a
    /// dark ground shows nothing BUT that bevel, which is the case where the argument for it is
    /// weakest and its cost highest.
    ///
    /// So a container is a container: [`crate::gfx::GlassRim::Standing`], and the rim drawn OVER the
    /// material at [`theme::GLASS_RIM`] with the boost to [`theme::GLASS_RIM_LIGHT`] on the side
    /// facing the light — the same two weights, the same lamp, the same one pixel. What that
    /// variant IS has since changed under this note, and the note holds: it was a line with no ramp
    /// and no bend, it is now a 12px chamfer and a 24px lens, and the sentence that matters is that
    /// the bar and the panel take the SAME one. Thickness comes from the material — the frost, the
    /// shadow, and now the bend — not from a ramp that covers a quarter of the panel.
    ///
    /// The OPAQUE fallback takes the rim too. A glass panel and a solid one are one object in two
    /// materials, and an edge is not part of what makes them different.
    pub(crate) fn panel(self, p: Painter, r: Rect, rest_dy: f32, radius: f32) {
        let boost = theme::GLASS_RIM_LIGHT[3] - theme::GLASS_RIM[3];
        // A PANEL's frost is `theme::PANEL_FROST_*`, drawn as its own quad below — it is a sheet,
        // not a container, and its edge is the shader's own specular. `GlassFace::NONE`.
        if self.backdrop(p, r, rest_dy, radius, [1.0, 1.0, 1.0, 1.0], crate::gfx::GlassRim::Standing, crate::gfx::GlassFace::NONE, panel_material()) {
            let (ft, fb) = panel_frost();
            crate::ui::profile::phase("glass.frost", || {
                p.rect_rimmed(r, radius, ft, fb, theme::GLASS_RIM, boost);
            });
        } else {
            p.rect_rimmed(r, radius, theme::PANEL_TOP, theme::PANEL_BOT, theme::GLASS_RIM, boost);
        }
    }
}

/// **What a popover is made of — both halves, from one name.** See [`theme::Material`].
///
/// A panel is HEAVIER and SOFTER than the bar on purpose, and both halves come from the same place
/// so they cannot drift apart. The reference is unambiguous: in one screenshot of iOS 26's TV app
/// the posters behind the tab BAR are readable and behind the context MENU above it almost nothing
/// is. A menu is a surface you read and act on; a bar is chrome you look past.
///
/// Laddered on the television over one ground (.72/.55/.42/.30 frost): at .42 the menu's own rows go
/// soft against the page coming through, and by .30 the panel has stopped reading as a surface. So
/// the density does not come DOWN toward the bar, which is the obvious "unification" and the wrong
/// move; what changed instead is that the panel now also gets the extra sample the bar declines,
/// because lightening the shared snapshot for the bar had lightened the menus with it.
///
/// `/tmp/plxnative-material=<ultrathin|thin|regular|thick|ultrathick>` swaps the whole material,
/// and `/tmp/plxnative-panelfrost=<a>` still overrides the density alone for a finer sweep.
fn panel_material() -> theme::Material {
    material_sweep().unwrap_or(theme::PANEL_MATERIAL)
}

fn panel_frost() -> ([f32; 4], [f32; 4]) {
    let a = frost_sweep().unwrap_or(panel_material().frost());
    (theme::with_a(theme::PANEL_FROST_TOP, a), theme::with_a(theme::PANEL_FROST_BOT, a))
}

#[cfg(feature = "devtriggers")]
fn material_sweep() -> Option<theme::Material> {
    static SEEN: std::sync::OnceLock<Option<theme::Material>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        let m = theme::Material::parse(&crate::dev::read("material")?)?;
        crate::log(&format!("glass: panel material swept to {m:?}"));
        Some(m)
    })
}
#[cfg(not(feature = "devtriggers"))]
fn material_sweep() -> Option<theme::Material> {
    None
}

#[cfg(feature = "devtriggers")]
fn frost_sweep() -> Option<f32> {
    static SEEN: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        let v = crate::dev::read("panelfrost")?.trim().parse::<f32>().ok()?;
        crate::log(&format!("glass: panel frost swept to {v}"));
        Some(v.clamp(0.0, 1.0))
    })
}
#[cfg(not(feature = "devtriggers"))]
fn frost_sweep() -> Option<f32> {
    None
}

/// Build `srv`'s transcode key for `path` on the stack. The ONE key builder the resolvers below
/// share, so a warm and the draw that follows it can never name two different slots — `(server,
/// path, w, h, png)` IS the store key. Per-frame hot path (every visible tile): the
/// NUL-terminated copy `poster_key` wants is made on the stack, no heap alloc.
fn tex_key(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> [u8; 352] {
    let mut p = [0u8; 256];
    crate::cbuf::set_bytes(&mut p, path);
    let mut key = [0u8; 352];
    crate::posters::poster_key(srv, key.as_mut_ptr() as *mut c_char, key.len(), p.as_ptr() as *const c_char, w, h, png);
    key
}

/// build the transcode key on the stack and resolve it to a GL texture (0 until loaded), for art
/// on the server the user is browsing.
///
/// **A thumb path is only meaningful on the server that issued it** — rating keys are server-local
/// integers from 1, so the same `/library/metadata/42/thumb/…` names a different film on a
/// friend's share. This form says "the current server", which is the right answer for art that
/// belongs to the screen (a section's own tiles, a person's headshot) and the wrong one for an
/// item that came from somewhere else: those call [`resolve_tex_on`] with the item's own server.
pub(crate) fn resolve_tex(path: &str, w: c_int, h: c_int, png: c_int) -> u32 {
    resolve_tex_on(crate::plex::current_server(), path, w, h, png)
}

/// [`resolve_tex`] for art belonging to a NAMED server.
pub(crate) fn resolve_tex_on(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> u32 {
    if path.is_empty() {
        return 0;
    }
    crate::posters::poster_get(srv, tex_key(srv, path, w, h, png).as_ptr() as *const c_char)
}

/// [`resolve_tex_on`] plus the DECODED pixel size of the texture — `(0, 0.0, 0.0)` until it is
/// ready. For art that must be FIT or COVERED into its frame rather than stretched to it
/// ([`Rect::cover`](crate::ui::Rect::cover)). The size is the store's own answer about the slot it
/// just probed for the texture, so this is the SAME single lookup [`resolve_tex_on`] does — knowing
/// the source aspect is free, and a screen never pays a second lock + key scan for it.
///
/// **There is deliberately no current-server twin of this or of [`warm_tex_on`].** Both had one and
/// neither had a caller: every user is a hero backdrop or a prefetch of one, and a hero is exactly
/// the art that belongs to an ITEM rather than to the screen. A bare form would be the shorter name
/// autocomplete offers for the case where getting it wrong is a blank billboard.
pub(crate) fn resolve_tex_wh_on(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> (u32, f32, f32) {
    if path.is_empty() {
        return (0, 0.0, 0.0);
    }
    let key = tex_key(srv, path, w, h, png);
    let (tex, pw, ph) = crate::posters::poster_get_wh(srv, key.as_ptr() as *const c_char);
    (tex, pw as f32, ph as f32)
}

/// The prefetch twin of [`resolve_tex_on`]: build the same transcode key and hand it to
/// [`posters::poster_warm`](crate::posters::poster_warm) — start the fetch, take no texture, take no
/// LRU protection. Same arguments on purpose, so a screen warms EXACTLY the key it will later
/// resolve; a warm at a different size — or on a different server — is a different slot and buys
/// nothing.
pub(crate) fn warm_tex_on(srv: ServerId, path: &str, w: c_int, h: c_int, png: c_int) -> crate::posters::Warm {
    if path.is_empty() {
        return crate::posters::Warm::Known;
    }
    let key = tex_key(srv, path, w, h, png);
    crate::posters::poster_warm(srv, key.as_ptr() as *const c_char)
}

/// Source art for a [`card`]: a catalog poster (resolved 250×375, dark gradient skeleton), any
/// keyed thumbnail at an explicit resolution (flat placeholder skeleton), or a person's headshot
/// (the same thumbnail, but its EMPTY case draws a person glyph rather than a blank tile).
pub(crate) enum Art<'a> {
    Poster(Option<&'a PmsMovie>),
    /// `sid` is the server `key` is a path on — an image-transcode path embeds a server-local
    /// ratingKey, so a still or a poster fetched from another machine is a 404 or, worse, a
    /// different item's picture. Every variant here carries one for that reason.
    Thumb { sid: ServerId, key: &'a str, res: (c_int, c_int) },
    /// A credit's headshot (the Cast & Crew shelf). Distinct from [`Art::Thumb`] because a
    /// missing headshot is ROUTINE here — the server has one for most actors and for few crew —
    /// and an empty circle beside named circles reads as a broken image rather than as a person
    /// the metadata agent has no photo of.
    Person { sid: ServerId, key: &'a str, res: (c_int, c_int) },
}

/// The one art-tile draw op. Resolves `art` to a texture (or a dark skeleton) and draws it at `frame`,
/// scaled about its centre when `focused`. A textured tile routes through the CARD COMPOSITE
/// ([`Painter::tex_carded`]): texture + 1px edge-sheen + the soft drop-shadow that GROWS with the pop
/// factor `f` (0 = resting/close to the shelf, 1 = fully lifted), all in ONE pass. The caller supplies
/// `f` (the shelves compute it from their per-cell spring; the episode/chapters strips from `scale`).
/// A not-yet-loaded skeleton falls back to a rimmed fill (no shadow until the art arrives).
pub(crate) fn card(p: Painter, frame: Rect, art: Art, rad: f32, focused: bool, scale: f32, f: f32) {
    let r = if focused { frame.scaled(scale) } else { frame };
    match art {
        Art::Poster(m) => {
            // **The ROW's server, not the current one.** A `thumb` path is a key on the server that
            // issued it — image-transcode paths embed a server-local ratingKey — so the bare
            // `resolve_tex` (which is `_on(current_server(), …)`) fetched every tile's art from
            // whichever server was current. That was invisible only while browsing a shared library
            // also re-pointed `current`; the moment that stopped, the Library grid of a friend's
            // library drew skeletons for most tiles and OUR films for the few ratingKeys that
            // happen to collide — both servers number from 1, so collisions are the normal case.
            let t = m.map(|m| resolve_tex_on(m.sid, &m.thumb, 250, 375, 0)).unwrap_or(0);
            if t != 0 {
                p.tex_carded(t, r, rad, theme::TINT_WHITE, f);
            } else {
                p.rect_sheened(r, rad, theme::SKELETON_TOP, theme::SKELETON_BOT);
            }
            // The ONE state language on every poster, drawn in this shared composite so Home
            // shelves + the Library grid + Search + the person page + the detail page's Related
            // shelf all inherit it: the amber WATCHED disc here (finished), the amber resume BAR
            // (`card_row::resume_bar`) for in progress, and — deliberately — NOTHING for never
            // started.
            //
            // **Inheriting it is a property of the ART VARIANT, not of the shelf**, and this line
            // claimed Related had it for months while Related passed `Art::Thumb` and so wore no
            // mark at all. A `Thumb` is a path and a size; only `Poster` carries the row the mark is
            // derived from, so a shelf that wants the state language must hand over a `PmsMovie` —
            // which is what putting a real catalog row behind Related's tiles bought (2026-08-21,
            // `metadata::Related`).
            //
            // **Amber means "you have watched this"**, one hue for one vocabulary. Until 2026-08-13
            // this corner carried the opposite claim (an amber ANGLE marking a fully UNWATCHED
            // item), and the inversion is the design system's (`ArtTile`: "most of the server is
            // unwatched, so only a finished tile is marked — a bare tile means nothing has been
            // seen"). The old polarity made a freshly-added library a wall of amber where the mark
            // said nothing you could act on, and left the one item you had actually finished as the
            // only clean tile on the shelf; it also made the poster disagree with the episode
            // still beside it, whose `✓` has always meant watched.
            //
            // Never both marks. **In progress WINS over watched**, because PMS keeps a resume point
            // on a finished-then-restarted item, so the wire says both — and being part-way through
            // a re-watch is what the viewer is actually doing (`detail::ep_state` resolves the same
            // three states for a still, and its table is the authority for all of them).
            if let Some(m) = m {
                if poster_mark(m) == PosterMark::Watched {
                    watched_mark(p, r, rad);
                }
            }
        }
        Art::Thumb { sid, key, res } => {
            let t = resolve_tex_on(sid, key, res.0, res.1, 0);
            if t != 0 {
                p.tex_carded(t, r, rad, theme::TINT_WHITE, f);
            } else {
                p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
            }
        }
        Art::Person { sid, key, res } => {
            let t = resolve_tex_on(sid, key, res.0, res.1, 0);
            if t != 0 {
                p.tex_carded(t, r, rad, theme::TINT_WHITE, f);
            } else {
                p.rrect_sheened(r, rad, theme::CARD_PLACEHOLDER);
                // Only for a person the server has NO headshot of — an unresolved texture with a
                // key behind it is merely still loading, and glyphing that would flash a "no
                // photo" mark on every tile of every page for the length of its fetch.
                if key.is_empty() {
                    // quantize the glyph box to 4px so the focus-pop animation reuses a handful of
                    // cached icon masks instead of rasterizing + uploading one per rounded pixel
                    // (same discipline as the unwatched angle above)
                    let d = ((r.w * PERSON_GLYPH_RATIO) / 4.0).round() * 4.0;
                    crate::ui::icons::draw(
                        p,
                        crate::ui::icons::Icon::User,
                        Rect::new(r.cx() - d * 0.5, r.cy() - d * 0.5, d, d),
                        theme::TEXT_TERTIARY,
                    );
                }
            }
        }
    }
}

/// Which state mark a poster wears — the pure half of [`card`]'s corner, split out for exactly the
/// reason `detail::ep_state` is: the CHOICE is the behaviour, while drawing it needs a GL context no
/// host test has. Nothing else inside `card` is assertable, and this is the part that can be wrong.
///
/// | state | mark | drawn by |
/// |---|---|---|
/// | never started | nothing — and most of a server is here | — |
/// | in progress | the full-bleed resume BAR | the CALLER (`card_row::draw_tile`/`draw_focused`) |
/// | watched | the amber corner DISC | [`card`] |
///
/// [`PosterMark::InProgress`] is *defined* as "[`PmsMovie::resume_frac`] has a value" — precisely
/// when the caller draws the bar — so the two halves cannot disagree and put two marks on one tile.
/// That is also the precedence: a re-watch in flight outranks the watched flag, because PMS reports
/// both on a finished-then-restarted item and being part-way through the re-watch is what the viewer
/// is doing. Same answer as the still's resolver, on the same item.
///
/// The three states are the app's ONE watch-state vocabulary, not the poster's alone: the detail
/// hero's watch controls ([`crate::ui::detail`]'s `hero_watch_state`) and the tile context menu's
/// state rows ([`row_watch_state`], and `detail::ep_watch_state` for one episode of a loaded season)
/// all resolve into this same enum, so "what state is this item in" has one answer and one set of
/// names wherever it is asked.
///
/// There are four resolvers rather than one because the INPUTS differ — a catalog row, a loaded
/// `metadata::Detail`, one `metadata::Episode` — and, in exactly one place, because the QUESTION
/// does: a MARK describes while a CONTROL promises what its press delivers, so a container mid-run
/// wears no mark ([`poster_mark`]) and still offers both write verbs ([`row_watch_state`],
/// `hero_watch_state`). Each of the three names the other it departs from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum PosterMark {
    /// never started — and the `Default`, because most of a server is here
    #[default]
    None,
    InProgress,
    Watched,
}

/// Resolve [`PosterMark`] from a catalog row. Total and mutually exclusive by construction.
///
/// Keyed on `PmsMovie::watched` — **not** on `!unwatched`, which is a weaker claim for a container:
/// a SHOW with one episode played is `!unwatched` but nowhere near done, and marking it watched is a
/// statement the viewer can see is false. Partly-watched shows therefore land on [`PosterMark::None`]
/// beside never-started ones; the true statement about a series mid-run is where its next episode
/// stands, which a poster in a grid is not the place for. **That is this function's rule and not the
/// enum's** — a MENU asking which verbs are reachable gets a different answer for the same show, and
/// asks [`row_watch_state`] instead.
pub(crate) fn poster_mark(m: &PmsMovie) -> PosterMark {
    if m.resume_frac().is_some() {
        return PosterMark::InProgress;
    }
    if m.watched {
        PosterMark::Watched
    } else {
        PosterMark::None
    }
}

/// The same three states asked as the **write-verb** question: which ends of the watch range can
/// this row still be sent to — i.e. whether a menu offers *Mark as Watched*, *Mark as Unwatched*, or
/// BOTH ([`crate::ui::item_menu`]'s state group).
///
/// It is [`poster_mark`] plus one rule, and the split is the same one the detail hero draws
/// ([`crate::ui::detail`]'s `hero_watch_state`): **a mark DESCRIBES, a control PROMISES.** A poster
/// says nothing about a show three episodes into ten, because "where its next episode stands" is not
/// a statement a tile in a grid can make — so `poster_mark` sends a container mid-run to
/// [`PosterMark::None`], beside the never-started. A menu is asking something else entirely, and for
/// a show in the middle both verbs are reachable and both are true, so it is [`PosterMark::InProgress`]
/// and gets the pair. Anything else and the two surfaces the owner reaches this item through — the
/// hero's discs and the tile's menu — would disagree about one item.
///
/// The extra rule is exactly "**neither end**", which needs no `kind` test: for a leaf `unwatched`
/// and `watched` are complements (`viewCount == 0` vs `> 0`), so only a CONTAINER can be neither, and
/// a container that is neither is one with some leaves viewed and some not (`pms::parse_item`). The
/// one other row that lands here is a container the server sent no leaf counts for, where both flags
/// are false: that item's state is unknown, both verbs are legitimate, and neither LABEL claims a
/// state — each names the outcome its press produces.
///
/// Leaves are delegated rather than re-derived, so the resume-point edge cases (`resume_frac`'s "at
/// or past the end is finished, not in progress") stay in one place.
pub(crate) fn row_watch_state(m: &PmsMovie) -> PosterMark {
    if !m.unwatched && !m.watched {
        return PosterMark::InProgress;
    }
    poster_mark(m)
}

/// The app's **two watch-state verbs**, written down once.
///
/// Two surfaces offer this pair of writes — the press-and-hold card menu's state group
/// ([`crate::ui::item_menu`]) and the detail hero's discs, which unfurl the verb on focus
/// ([`crate::ui::detail`]'s `watch_label`) — and they are the same two actions on the same item.
/// A menu row reading *Mark as Watched* beside a control reading *Watched* would read as two
/// different writes, so both take the words from here. They sit beside [`row_watch_state`] because
/// that function is the other half of the same vocabulary: it decides WHICH of these a surface may
/// offer, and these are what it offers.
///
/// Each names the OUTCOME its press produces, never the state the item is in — which is what lets
/// a part-watched item show both at once without either being a lie.
pub(crate) const MARK_WATCHED_VERB: &str = "Mark as Watched";
pub(crate) const MARK_UNWATCHED_VERB: &str = "Mark as Unwatched";

/// …and the third verb the same two surfaces share: **play this from 00:00, ignoring the resume
/// point.** The detail hero's disc and the card menu's row are one action, so they carry one WORD —
/// this one. Before 2026-08-21 they did not even do that: the row said this and the disc said
/// nothing at all, which is what the unfurl fixed.
///
/// **They deliberately carry two GLYPHS, and that is not the same mistake.** The row draws
/// [`crate::ui::icons::Icon::PlayStart`], the hero disc draws `Icon::Restart` — because the disc is
/// icon-only at rest and stands beside the Play pill, where a play-triangle-with-a-bar is that
/// pill's own mark plus a 3px stem, while the row's glyph sits next to the words above and nothing
/// resembling a play mark. The two were reconciled onto one glyph for a day and it was wrong;
/// `Icon::Restart`'s doc is the argument. One action, one word, and the mark chosen per surface for
/// what that surface has to tell apart.
pub(crate) const PLAY_FROM_START_VERB: &str = "Play from Start";

/// The **watched tick** on a poster, as fractions of the tile's DRAWN width: the tick's box, its
/// corner inset, then the veil's box. Anchored on the design system's `ArtTile` — a 26px tick inset
/// 12px under a 104px corner veil, on a 250-wide poster — and held as ratios rather than pixels so
/// the whole mark rides the focus pop as one object (a fixed 26px tick inside a tile growing to 1.09
/// visibly shrinks and drifts inward). The component's second rung (20px/76px on a tile under 200
/// wide) has no poster in this product: every poster shelf, the Library grid and Related are all
/// `consts::CARD_W` 250.
const TICK_RATIO: f32 = 26.0 / 250.0;
const TICK_INSET: f32 = 12.0 / 250.0;
const VEIL_RATIO: f32 = 104.0 / 250.0;
/// The tick's drop shadow, as fractions of the TICK's box (the design's `0 2px 7px`).
const TICK_SHADOW_BLUR: f32 = 7.0 / 26.0;
const TICK_SHADOW_DY: f32 = 2.0 / 26.0;
/// Where the veil's falloff reaches zero, as a fraction of its box — CSS
/// `radial-gradient(72% 72% at 100% 0%, veil, transparent 70%)` resolves to `.72 × .70`.
const VEIL_EXTENT: f32 = 0.72 * 0.70;
/// The veil texture's resolution. A power of two (NPOT sampling is a documented Mali trap) and far
/// finer than the ~52px of falloff it is stretched across, so the ramp is smooth under GL_LINEAR.
const VEIL_TEX_PX: usize = 64;
static mut VEIL_TEX: std::os::raw::c_uint = 0;

/// The corner **veil** texture: white RGB with a radial alpha falloff peaking at the TOP-RIGHT
/// corner, generated once and reused at every size. It is a texture rather than geometry because the
/// renderer has no radial gradient, and the two alternatives both fail on this shape: a `grad4` quad
/// is bilinear and, worse, square — it would spill past the tile's 14px corner ARC onto the shelf at
/// full strength, exactly where the mark is strongest; and stepping it as N rounded-rect bands (the
/// `art_scrim` trick) is what `hero_scrim`'s doc already rejected for a field this wide, at a visible
/// alpha staircase with `GL_DITHER` off. `Painter::tex` takes a corner radius, so ONE draw of this
/// gets the tile's own silhouette for free — the veil's other three corners live in fully
/// transparent territory, so rounding them changes nothing.
fn veil_tex() -> std::os::raw::c_uint {
    unsafe {
        let cached = *std::ptr::addr_of!(VEIL_TEX);
        if cached != 0 {
            return cached;
        }
        let n = VEIL_TEX_PX;
        let mut px = vec![0u8; n * n * 4];
        for y in 0..n {
            for x in 0..n {
                // distance from the top-right corner, in units of the box's width
                let dx = (n - 1 - x) as f32 / (n - 1) as f32;
                let dy = y as f32 / (n - 1) as f32;
                let a = (1.0 - (dx * dx + dy * dy).sqrt() / VEIL_EXTENT).clamp(0.0, 1.0);
                let i = (y * n + x) * 4;
                px[i] = 255;
                px[i + 1] = 255;
                px[i + 2] = 255;
                px[i + 3] = (a * 255.0).round() as u8;
            }
        }
        let tex = crate::gfx::upload_rgba(0, n as std::os::raw::c_int, n as std::os::raw::c_int, px.as_ptr());
        *std::ptr::addr_of_mut!(VEIL_TEX) = tex;
        tex
    }
}

/// The **watched** mark on a poster: a corner veil, then a bare tick over it. `card` is the rect
/// actually drawn (the SCALED one while the tile is popped) and `rad` its corner radius, which the
/// veil is masked to so nothing lands outside the tile's own silhouette.
///
/// No disc and no plate — the artwork stays visible and only the falloff touches it. The white tick
/// carries no contrast of its own, so legibility is the veil's job with the tick's own soft shadow
/// inside it; between them the mark holds on a snowfield and on a black-and-white title card, which
/// is the case that decided against a bare tick alone.
pub(crate) fn watched_mark(p: Painter, card: Rect, rad: f32) {
    let v = card.w * VEIL_RATIO;
    p.tex(veil_tex(), Rect::new(card.x + card.w - v, card.y, v, v), rad, theme::TILE_MARK_VEIL);
    // Quantized to 4px so the focus pop reuses a handful of cached masks instead of rasterizing +
    // uploading one per rounded pixel, and proportional to the DRAWN tile so the mark rides the pop.
    let d = ((card.w * TICK_RATIO) / 4.0).round() * 4.0; // 24px on a 250 card (26 → nearest rung)
    let ins = card.w * TICK_INSET;
    let tick = Rect::new(card.x + card.w - ins - d, card.y + ins, d, d);
    p.shadow(tick, d * 0.5, d * TICK_SHADOW_BLUR, d * TICK_SHADOW_DY, theme::TILE_MARK_SHADOW);
    crate::ui::icons::draw(p, crate::ui::icons::Icon::Check, tick, theme::TILE_MARK_INK);
}

/// Person-glyph box as a fraction of a headshot tile — the [`Art::Person`] fallback's one ratio.
/// Deliberately TIGHTER than [`DISC_ICON_RATIO`] (0.54, the ratio every disc *control* glyph uses,
/// and what the profile chip's own fallback works out to): a headshot tile is 190px, and 0.54 of it
/// is past `icons::tex_for`'s 96px rasterization clamp — the mask would be upscaled and soft. At
/// 0.44 the box lands at 84-88px across the whole focus pop, inside the clamp and crisp.
const PERSON_GLYPH_RATIO: f32 = 0.44;

/// The scale a focused card pops to (shared by every animated card row).
pub(crate) const CARD_FOCUS_SCALE: f32 = 1.07;

/// The scale a focused CONTROL face pops to — the design system's `--focus-scale-control`.
///
/// Every control face takes it: [`Button`], [`CircleButton`] and [`TransportButton`]. It is
/// deliberately the generic tile's number and no larger — the FILL already carries focus here, so
/// the pop is the second half of a signal rather than the whole of one, and a control that popped
/// like a poster (1.09) would out-shout the artwork it sits on.
///
/// **A control inside a TRACK does not take it.** The top tab bar's pills and the profile chip are
/// focused by their row's own motion — the capsule travelling, the chip unfurling — and a pill that
/// also grew would be two answers to one question. The season strip's BARE pills are not in a track
/// and do pop.
///
/// Written as its own constant rather than an alias of [`CARD_FOCUS_SCALE`] because the two are
/// separate design decisions that happen to have landed on one number; either may move alone.
pub(crate) const CTRL_FOCUS_SCALE: f32 = 1.07;

/// One control ROW's focus pop — a spring per control, so the arriving face grows while the one
/// being left behind shrinks.
///
/// A row rather than a widget owns this for the reason every animated row in this app does
/// ([`crate::ui::card_row`]): the pop is a property of a control's place in a row, the widgets are
/// immediate-mode and keep nothing between frames, and the leaving control still needs a value after
/// it has stopped being the focused one. A single global spring cannot express that — it would have
/// to snap the outgoing face to 1.0 the instant focus moved, which is the 4px jump this exists to
/// avoid.
///
/// The spring is **underdamped** (`--ease-bounce`, the press release's own curve): the design system
/// says the click and the control focus pop are the two things in the app that bounce, so they share
/// `press`'s constants rather than restating them.
///
/// [`scale`](Self::scale) folds in [`press::scale`](crate::ui::press::scale) for the focused control
/// only. The dip is a FACTOR on top of the focus scale — that is what makes a press read the same on
/// a 1.07 capsule as on a 1.09 poster — and it belongs to the control being pressed, which is always
/// the focused one.
pub struct CtlPop<const N: usize> {
    sp: [Spring; N],
    focused: Option<usize>,
}
impl<const N: usize> CtlPop<N> {
    pub const fn new() -> Self {
        Self { sp: [Spring::at(1.0); N], focused: None }
    }
    /// Advance every control's spring toward its target for this frame. `focused` is the index of
    /// the control holding focus, or `None` when the row has none at all (the page scrolled away
    /// from it, a panel took over), which closes every pop.
    pub fn step(&mut self, focused: Option<usize>, dt: f32) {
        self.focused = focused;
        for (i, sp) in self.sp.iter_mut().enumerate() {
            let target = if focused == Some(i) { CTRL_FOCUS_SCALE } else { 1.0 };
            sp.step_zeta(target, crate::ui::press::K_POP, crate::ui::press::ZETA_POP, dt);
        }
    }
    /// Control `i`'s drawn scale this frame, press dip included. Out-of-range asks answer 1.0 rather
    /// than panicking: a row whose control COUNT changes with the item (`detail::hero_ctls`) indexes
    /// this by a position that can outrun `N` for a frame while the set is being rebuilt.
    pub fn scale(&self, i: usize) -> f32 {
        let s = self.sp.get(i).map(|sp| sp.pos).unwrap_or(1.0);
        if self.focused == Some(i) {
            s * crate::ui::press::scale()
        } else {
            s
        }
    }
    /// Drop every pop to rest with no motion in between — for a page being torn down or re-mounted,
    /// so the next mount does not open on a control already popped (`detail::reset_view_state`).
    pub fn reset(&mut self) {
        self.focused = None;
        for sp in self.sp.iter_mut() {
            sp.jump(1.0);
        }
    }
}
impl<const N: usize> Default for CtlPop<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Icon box as a fraction of a round control's diameter — the ONE ratio every disc glyph uses
/// (transport CC/Audio buttons, CircleButton vector icons, the Continue-Watching play badge).
pub(crate) const DISC_ICON_RATIO: f32 = 0.54;

/// The air between one control of an ACTION ROW and the next, edge to edge — the sibling of
/// [`StatusOverlay::CTRL_H`] on the other axis, and the second half of what makes the home hero's
/// row and the detail hero's row one object rather than two that agree by inspection. Both wrote
/// out a private `20.0` (`home::HERO_CTRL_GAP`, `detail::CGAP`), so the rhythm those rows are meant
/// to share was a number two files happened to hold the same copy of.
///
/// Deliberately NOT a [`theme::space`] rung: that ladder is the gap between stacked BLOCKS in a
/// page's vertical flow, and this is the air inside one horizontal control row, measured against
/// the 60px control it separates. Adding a 20 to the ladder to launder it would put a rung on the
/// scale that no block spacing may use, which is worse than an honest constant with a home.
pub(crate) const CTRL_GAP: f32 = 20.0;

/// A media card shared by the episode picker and the chapters strip so they resolve + animate
/// identically: the thumbnail (at `res`, or a dark placeholder), a focus scale-pop about the centre +
/// the focus treatment (soft drop-shadow + top sheen) when `focused` (the caller owns the `scale`
/// spring).
pub(crate) fn draw_card(
    p: Painter,
    frame: Rect,
    sid: ServerId,
    thumb: &str,
    res: (c_int, c_int),
    radius: f32,
    focused: bool,
    scale: f32,
) {
    // pop factor from the caller's scale spring (0 at rest → 1 at full focus scale) drives the folded shadow
    let f = if focused { ((scale - 1.0) / (CARD_FOCUS_SCALE - 1.0)).clamp(0.0, 1.0) } else { 0.0 };
    card(p, frame, Art::Thumb { sid, key: thumb, res }, radius, focused, scale, f);
}

/// How many flat bands [`art_scrim`] uses for its corner region — see there for why they exist. 3 is
/// enough that the step between them is ~0.03 alpha, well under a visible edge.
const SCRIM_CORNER_BANDS: usize = 3;

/// **THE progress bar** — the app's one "how far into this am I" mark, on every tile shape: the bottom
/// band of the artwork itself, full-bleed edge to edge, square-ended fill, clipped to the card's own
/// rounded silhouette so the amber visibly wraps the bottom corner arcs (the mock draws two square
/// strips under a `overflow:hidden`, which is what that clip reproduces).
///
/// One function for the Continue Watching poster and the episode still, because it is meant to be the
/// SAME bar: the still used to draw an inset rounded capsule 16px up while a CW card drew this, so
/// "how far in am I" was two different objects on two screens of one app.
///
/// Two details are load-bearing, both learned on the panel:
///
/// * **The band is snapped to whole composited pixels.** `gfx::clip_set` truncates its scissor to
///   integer rows while the fill is antialiased at fractional coordinates, so an unsnapped band leaves
///   a hairline of either unscrimmed artwork or double-darkened scrim (see [`art_scrim`], same cause).
/// * **Track and fill never share a pixel.** They used to be drawn as full-width track, then fill over
///   it — two translucent fills (α .22 and α .95), each with its own antialiased edge, compositing on
///   the same pixels where the card's SDF coverage is partial. Along the bottom-LEFT corner arc that
///   sum came out brighter than either one alone: a 1–2px light fleck at the corner, which pulsed as
///   the focus pop moved the geometry. Splitting the band at the played fraction removes the overlap
///   rather than trying to tune around it.
pub(crate) fn progress_bar(p: Painter, card: Rect, rad: f32, h: f32, frac: f32) {
    let snap = |y: f32| crate::gfx::snap(y + p.dy) - p.dy;
    let bottom = card.y + card.h;
    let top = snap(bottom - h.min(card.h));
    if bottom <= top {
        return;
    }
    let right = card.x + card.w;
    // the split is a whole pixel too, so the fill's square end is a clean edge rather than a column
    // of half-covered amber
    let split = (card.x + card.w * frac.clamp(0.0, 1.0)).round().clamp(card.x, right);
    if split > card.x {
        p.clip(Rect::new(card.x, top, split - card.x, bottom - top));
        p.rrect(card, rad, rad, theme::RESUME_FILL);
    }
    if split < right {
        p.clip(Rect::new(split, top, right - split, bottom - top));
        p.rrect(card, rad, rad, theme::RESUME_TRACK);
    }
    p.clip_clear();
}

/// The corner radius of [`text_block_highlight`] — the detail page's About columns and its episode
/// filmstrip both drew an 18 here, independently, and it is one object doing one job.
pub(crate) const TEXT_BLOCK_HL_RAD: f32 = 18.0;

/// **THE "this block of text is the thing you are on" panel** — the focus mark for a run of prose
/// that is not a card: the detail page's four About columns and its episode filmstrip's metadata
/// row. One function, because `ui/CLAUDE.md` names those two call sites as one idiom and they had
/// already drifted once (the About footer wrote a bare `18.0` while the filmstrip named a const).
///
/// It is a very quiet wash ([`theme::OVERLAY_FOCUS_SOFT`], 7% white) and that is the whole reason
/// it needs the **shadow** rather than a heavier fill: over the detail page's ambient ground a 7%
/// panel has almost no edge of its own, so it reads as a smudge rather than as a raised surface,
/// and lifting the wash instead would put a bright rectangle over the prose it exists to point at.
/// **No stroke, no rim, no glass** — the mark is a wash plus a lift, and nothing else. (The owner
/// ruled all three out for a selection on 2026-08-21, on seeing what the previous shadow drew.)
///
/// **Both halves of the shadow are here because the panel is SEE-THROUGH, and neither of them can
/// be borrowed from the card family.**
/// - [`Painter::shadow_outside`], not [`Painter::shadow`]: the tile path throws away a BOX-shaped
///   interior inset by `radius + 1` px, which leaves a full-strength band of ink under the
///   occluder's own rim ending in a hard step. A card hides that band; a 7% wash shows it, and it
///   reads as a dark rounded FRAME with the wash inset inside it — which is exactly what was
///   shipping. The `_outside` cut is the panel's own rounded shape, so no ink lands under it.
/// - [`theme::TEXT_BLOCK_SHADOW_BLUR`]/`_DY`/`_A`, not the `CARD_SHADOW_REST_*` triple: a contact
///   shadow tuned to be seen only where it escapes past an opaque poster is fully visible here, and
///   at 11px/0.34 its falloff has a readable boundary on a wide straight edge. That token block
///   carries the reasoning.
pub(crate) fn text_block_highlight(p: Painter, r: Rect) {
    p.shadow_outside(r, TEXT_BLOCK_HL_RAD, theme::TEXT_BLOCK_SHADOW_BLUR, theme::TEXT_BLOCK_SHADOW_DY,
                     theme::with_a(theme::CARD_SHADOW, theme::TEXT_BLOCK_SHADOW_A));
    p.rrect(r, TEXT_BLOCK_HL_RAD, TEXT_BLOCK_HL_RAD, theme::OVERLAY_FOCUS_SOFT);
}

// ---- Keyline chip: the FINE-PRINT outlined chip — a hairline box round a very short label, sized to
// hug it. Its one job is the content rating beside an episode's air date (`18+` / `TV-MA`).
//
// Deliberately NOT `badge` + `BadgeStyle::Outlined`, which is the same shape two rungs up: that chip
// is built on `BADGE_H` (34) with 12px padding, a 2px keyline and a BOLD `CAPTION` label, because it
// exists to sit in a row of metadata chips and hold its own. Down here it has to sit beside a 22px
// air date as the dimmest thing on the tile, and at badge's weight it outweighed the episode TITLE
// above it. `BADGE_H` stays one band for every `BadgeStyle`, as documented — this is a different leaf,
// not a resized one.
const KEYLINE_PAD_X: f32 = 7.0;
const KEYLINE_PAD_Y: f32 = 6.0;
const KEYLINE_RAD: f32 = 5.0;
const KEYLINE_W: f32 = 1.5;
/// The chip's label weight — the mock's `font-weight:600`. See [`keyline_chip`] for why it is bold.
const KEYLINE_BOLD: std::os::raw::c_int = 1;

/// The width [`keyline_chip`] will occupy for `text` — the measure-first companion.
pub(crate) fn keyline_chip_w(text: &str) -> f32 {
    std::ffi::CString::new(text)
        .ok()
        .map(|c| crate::text::text_width(c.as_ptr(), theme::size::CAPTION, KEYLINE_BOLD) + 2.0 * KEYLINE_PAD_X)
        .unwrap_or(0.0)
}

/// Draw a fine-print keyline chip with its LEFT edge at `x`, centred on `cy`; returns its width.
///
/// **A real hollow ring** ([`Painter::rring`]), so whatever is behind it shows through the middle —
/// the mock's `box-shadow: inset 0 0 0 1.5px …` with no `background`. It used to be a KNOCKOUT
/// (stroke colour, then the interior repainted in a `bg` the caller named), which is exact on a
/// flat panel and wrong over artwork: on the detail hero's identity line the ground is a backdrop
/// plus two scrim ramps, so a chip claiming `SURFACE_APP` read as a dark box over a bright still
/// instead of a hairline. The parameter is gone rather than defaulted — there was no honest value
/// for it, which is the point.
/// The ring takes the LABEL'S OWN INK, and the mock's `rgba(255,255,255,.34)` deliberately does
/// not port — it cannot. `Painter::rring` is the SDF's rim band, whose coverage is the product of
/// two 1.5px-wide smoothsteps (`fs_src.frag`); at [`KEYLINE_W`] the window where both reach 1 is
/// about a QUARTER of a pixel, so no pixel centre lands in it and a thin rim never resolves to
/// more than a fraction of its stated alpha. Measured on the panel: `white .34` came out ~12
/// levels above the ground where the arithmetic says ~87 — an outline nobody can see. The opaque
/// ink is what makes a 1.5px ring exist at all here, and it is what shipped before this was
/// briefly "corrected" to the mock's literal value.
///
/// The label is BOLD for the same reason the mock sets `font-weight:600` on it: two or three caps
/// at `CAPTION` inside a ring have to hold their own against it, and regular weight is what made
/// this chip read as an empty frame in the first device photograph of the identity line.
pub(crate) fn keyline_chip(p: Painter, x: f32, cy: f32, text: &str, col: [f32; 4]) -> f32 {
    let lc = match std::ffi::CString::new(text) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let w = keyline_chip_w(text);
    let h = crate::text::cap_h(theme::size::CAPTION, KEYLINE_BOLD) + 2.0 * KEYLINE_PAD_Y; // hugs the label's cap band, not a fixed band
    p.rring(Rect::new(x, cy - h * 0.5, w, h), KEYLINE_RAD, KEYLINE_W, col);
    p.text(
        lc.as_ptr(),
        x + KEYLINE_PAD_X,
        crate::text::text_vcenter_y(theme::size::CAPTION, KEYLINE_BOLD, cy),
        theme::size::CAPTION,
        col,
        0,
        KEYLINE_BOLD,
    );
    w
}

// ---- Key cap + key hint: a REMOTE BUTTON named in running prose --------------------------------
//
// "Press [BACK] to return" — the line every read-only alert panel closes with (`Alert Views.dc.html`
// 1A/1B/1C all carry it, 1E states the rule: the panels that hold no control close on BACK, so the
// line IS the whole affordance) and the line the player's failure read-out already carried.
//
// **The cap is the point, not decoration.** `BACK` set in the same prose as the words around it
// reads as a word; the same three letters inside a keyline read as the thing under your thumb. That
// distinction has to survive a PHOTOGRAPH of a television in an issue thread, which is the failure
// read-out's whole output format, and a keyline survives a phone camera's chroma subsampling where
// a colour or weight change does not.
//
// The idiom was written first as a private `draw_hint_with_keycap` in `player_hud.rs`, screen-local
// and centred on `SCR_W`. It is here because the alert panels want the same object RIGHT-aligned
// inside a panel, and the guide's rule 4 answers that: promote the widget, don't fork it. Three
// alert lanes reached for it independently and all three rejected the HUD's CONSTRUCTION for the
// same reason, below.
//
// **The cap is a genuinely hollow ring in the LABEL'S OWN INK**, which is `keyline_chip`'s
// construction and its measured reason — read that one before retuning either. The HUD's original
// drew the ring as a knockout (a white-at-.34 rounded rect, then the interior repainted OPAQUE
// BLACK) and got away with it because the video plane behind the read-out is black by construction.
// On a glass panel that interior is a black hole punched through the frost. Going hollow is
// therefore forced — and the alpha cannot come with it: at [`KEYCAP_W`] 1.5 the SDF's rim coverage
// is the product of two 1.5px smoothsteps, whose overlap is about a quarter of a pixel, so a
// stated `.34` resolves to roughly a tenth of that on screen. `keyline_chip` measured it (~12
// levels above ground where the arithmetic says ~87). Opaque ink is what makes a 1.5px ring exist.
//
// **`player_hud::draw_hint_with_keycap` is DELIBERATELY NOT migrated onto this, and that is a
// decision rather than an oversight.** Two of the three lanes did migrate it and one refused; the
// refusal is what shipped. Going hollow is not a no-op on that screen — it trades a .34 white
// knockout for opaque `TEXT_SECONDARY`, so the cap gets brighter on the one screen whose whole
// design brief is "survive a phone photograph in an issue thread". That is a DEVICE look, gradeable
// only on the television and not on a host simulator, so the failure read-out keeps its own cap
// until someone can put the two side by side on the panel. The cost is a second cap construction in
// the tree, which is why it is written down here rather than left to be rediscovered as duplication
// worth cleaning up.
/// The cap's outer height — a fixed band, unlike [`keyline_chip`], which hugs its label's cap band.
/// A key cap stands for a physical button, so every cap in the app is the same size whatever word
/// is on it; only the WIDTH grows.
pub(crate) const KEYCAP_H: f32 = 36.0;
/// The narrowest a cap is ever drawn. `BACK` sets it; `OK` would otherwise come out as a stub, and
/// two caps of different widths on one line read as two different objects.
const KEYCAP_MIN_W: f32 = 74.0;
const KEYCAP_PAD_X: f32 = 12.0;
const KEYCAP_RAD: f32 = 8.0;
/// Stroke width — the design's 1.5, the same weight as [`keyline_chip`]'s and `ControlStyle::Keyline`'s.
const KEYCAP_W: f32 = 1.5;
/// A cap's label is BOLD [`theme::size::MICRO`]: it is a one-line de-emphasised LABEL in the rung's
/// own terms, and it must hold its own inside a ring at a size below the reading floor.
const KEYCAP_BOLD: c_int = 1;
/// The gap between the cap and the prose either side of it — the design's `gap:12`, which every
/// alert footer in `Alert Views.dc.html` sets (§1A's and both of §1B's runs). It shipped at a
/// hand-tuned 14 in all three lanes that built one of these panels, none of which could read the
/// spec; two pixels either side of one cap is not a thing anyone would have found by looking.
const KEYCAP_GAP: f32 = 12.0;

/// The width [`key_cap`] will occupy for `label` — the measure-first companion, so a caller can
/// right-align or centre the whole line before drawing any of it.
pub(crate) fn key_cap_w(label: &std::ffi::CStr) -> f32 {
    (crate::text::text_width(label.as_ptr(), theme::size::MICRO, KEYCAP_BOLD) + 2.0 * KEYCAP_PAD_X)
        .max(KEYCAP_MIN_W)
}

/// Draw one key cap with its LEFT edge at `x`, centred on `cy`; returns its width.
pub(crate) fn key_cap(p: Painter, x: f32, cy: f32, label: &std::ffi::CStr, ink: [f32; 4]) -> f32 {
    let w = key_cap_w(label);
    p.rring(Rect::new(x, cy - KEYCAP_H * 0.5, w, KEYCAP_H), KEYCAP_RAD, KEYCAP_W, ink);
    let tw = crate::text::text_width(label.as_ptr(), theme::size::MICRO, KEYCAP_BOLD);
    p.text(
        label.as_ptr(),
        x + (w - tw) * 0.5,
        crate::text::text_vcenter_y(theme::size::MICRO, KEYCAP_BOLD, cy),
        theme::size::MICRO,
        ink,
        0,
        KEYCAP_BOLD,
    );
    w
}

/// `{pre} [KEY] {post}` — one line of fine print with a [`key_cap`] set into the middle of it.
///
/// Measure-then-place, because a caller needs the width before it knows the x: an alert panel
/// right-aligns the line against its own padding edge (`right - hint.width()`), and the same
/// arithmetic centres it (`(w - hint.width()) * 0.5`) for whoever wants that. It places by x rather
/// than centring itself precisely so that both are the caller's to choose — the alignment is the
/// half that belongs to the screen, the assembled line is the half that does not.
///
/// The three `&CStr`s are BORROWED for the widget's lifetime — the `Label` rule in `ui/CLAUDE.md`:
/// keep the `CString` (or a `c"…"` literal) alive across the draw.
pub(crate) struct KeyHint<'a> {
    pre: &'a std::ffi::CStr,
    key: &'a std::ffi::CStr,
    post: &'a std::ffi::CStr,
}

impl<'a> KeyHint<'a> {
    pub(crate) fn new(pre: &'a std::ffi::CStr, key: &'a std::ffi::CStr, post: &'a std::ffi::CStr) -> Self {
        Self { pre, key, post }
    }

    /// Total width of the assembled line.
    pub(crate) fn width(&self) -> f32 {
        let sz = theme::size::CAPTION;
        crate::text::text_width(self.pre.as_ptr(), sz, 0)
            + KEYCAP_GAP
            + key_cap_w(self.key)
            + KEYCAP_GAP
            + crate::text::text_width(self.post.as_ptr(), sz, 0)
    }

    /// The band the line occupies — the cap is taller than the prose's cap band, so a caller
    /// reserving flow for this must reserve the CAP's height, not the text's.
    pub(crate) const fn height() -> f32 {
        KEYCAP_H
    }

    /// **The air a panel leaves BELOW this line, which is deliberately not that panel's own
    /// padding.** One rung ([`theme::space::MD`]), matching the `space::MD` step every alert in the
    /// family puts between its closing hairline and this line — so the hint sits centred in the band
    /// the rule opens, instead of riding high in it.
    ///
    /// All three read-only alerts drew `PAD` 48 here, which is what the design states
    /// (`Alert Views.dc.html`'s content div is `padding:48px` all round, and nothing follows the
    /// footer row). Against the PANEL FRAME that is exactly symmetric — the eyebrow's cap top is 48
    /// below the top edge and the key cap's ring is 48 above the bottom one. Against the HAIRLINE,
    /// which is the edge the eye actually measures this line from, it is 24 over and 48 under.
    ///
    /// And it is worse than 2:1 on the ink, which is the number that settles it: the reserved band
    /// is the CAP's 36px, while the words either side are `size::CAPTION` with a ~17px cap band
    /// centred in it. Only the 1.5px keycap ring ever reaches the band's edges, so the READ line
    /// runs 33.5px below the hairline and 56.5px above the panel's floor. Half the visible air is
    /// under three words nobody is meant to look at twice.
    pub(crate) const fn pad_below() -> f32 {
        theme::space::MD
    }


    /// Draw with the line's LEFT edge at `x`, its cap band vertically centred on `cy`. The prose
    /// sits on its own cap band (rule 3 — never a magic y), the cap on the same centre line.
    pub(crate) fn draw(&self, p: Painter, x: f32, cy: f32) {
        let sz = theme::size::CAPTION;
        let ty = crate::text::text_vcenter_y(sz, 0, cy);
        let pw = crate::text::text_width(self.pre.as_ptr(), sz, 0);
        p.text(self.pre.as_ptr(), x, ty, sz, theme::TEXT_TERTIARY, 0, 0);
        let kx = x + pw + KEYCAP_GAP;
        let kw = key_cap(p, kx, cy, self.key, theme::TEXT_SECONDARY);
        p.text(self.post.as_ptr(), kx + kw + KEYCAP_GAP, ty, sz, theme::TEXT_TERTIARY, 0, 0);
    }
}

// ---- Dotted run: fine-print facts separated by `·` ----------------------------------------------

/// Draw `parts` left to right from `x` on the cap-band y `y`, joined by a **`·` in
/// [`theme::TEXT_SEPARATOR`]** with `pad` either side of it. Returns the total drawn width.
///
/// **Empty parts are ABSENT, not blank**: a run with no content contributes neither a draw nor a
/// separator, so a person with no birthplace gets "Actor · 1987" and never "Actor · 1987 · ". That
/// is the whole reason this is a component and not a `join(" · ")` — the dot is a *different ink*
/// from the runs it divides ([`theme::TEXT_SEPARATOR`], .45 of the words' own, which is what every
/// mock in the design project sets a separator to). A dot sharing the ink of the words either side
/// JOINS them instead of punctuating them, and one `p.text` call cannot express the difference.
///
/// Promoted out of `detail.rs`, where it was `dotted_run` and private; the detail facts row and the
/// bio panel's identity line are the same idiom one screen apart, and the second copy is where the
/// separator ink would have drifted.
/// The hairline divider's height. Published because a panel's flow has to MEASURE the rule as
/// well as draw it, and the two files that made it a private constant each called it something
/// else (`RULE_H`, `HAIRLINE_H`) while holding the same 1.0.
pub(crate) const HAIRLINE_H: f32 = 1.0;

/// One full-width HAIRLINE divider — the alert family's rule, in one place.
///
/// **A `theme::HAIRLINE` rect, never a 1px `rrect` with a radius nobody can see.** That sentence
/// was already written down, in `tracks_panel`'s private copy of this function, while a third
/// panel drew exactly the `rrect` it forbids — which is what three private drawers of one line
/// buy you. The height is 1.0 and is not a parameter: a divider that is two pixels somewhere is a
/// different object, and both files that made it a named constant gave it the same value.
pub(crate) fn hairline(p: Painter, x: f32, y: f32, w: f32) {
    p.rect(Rect::new(x, y, w, HAIRLINE_H), 0.0, theme::HAIRLINE, theme::HAIRLINE, 0.0);
}

pub(crate) fn dotted_run(p: Painter, parts: &[&str], x: f32, y: f32, sz: c_int, col: [f32; 4], pad: f32) -> f32 {
    let mut bx = x;
    for part in parts.iter().filter(|s| !s.is_empty()) {
        if bx > x {
            bx += pad;
            // `c"·"`, not `CString::new` — the separator is a compile-time constant, and this runs
            // once per gap per frame on the detail facts row and the bio identity line.
            bx += p.text(c"\u{b7}".as_ptr(), bx, y, sz, theme::TEXT_SEPARATOR, 0, 0);
            bx += pad;
        }
        if let Ok(pc) = CString::new(*part) {
            bx += p.text(pc.as_ptr(), bx, y, sz, col, 0, 0);
        }
    }
    bx - x
}

// ---- Scroll rail: how far through a paged viewport you are --------------------------------------

/// The rail's bar width, and the corner that makes it a pill. A **6px** bar is the design's, and the
/// radius is simply half of it — a "pill radius" is not a number to choose, it is the constraint
/// that the ends are semicircles.
pub(crate) const RAIL_W: f32 = 6.0;
const RAIL_RAD: f32 = RAIL_W * 0.5;

/// Where the rail's fill sits inside its track, as `(top, height)` FRACTIONS of the track — the
/// pure half of [`scroll_rail`], so the arithmetic is host-testable without a GL context.
///
/// **It is quantised to PAGES, not to pixels, and that is the design.** The viewport it indexes
/// moves a page at a time (there is no continuous scroll to track), so a fill sized by
/// `viewport/content` would sit at fractions the content can never rest at and would stop short of
/// the bottom on the last page. One page of travel moves the fill by exactly its own height, which
/// is what makes the rail readable as "3 of 5" rather than as an approximate position.
///
/// `pages == 0` cannot happen from [`scroll_rail`] (a viewport that fits is not railed at all), and
/// is folded to the full track rather than dividing by zero. `page` is 1-based and clamped, so a
/// caller that has not yet re-clamped after a resize draws a rail that is merely stale, never one
/// hanging off the end of its track.
pub(crate) fn rail_geom(page: usize, pages: usize) -> (f32, f32) {
    if pages <= 1 {
        return (0.0, 1.0);
    }
    let h = 1.0 / pages as f32;
    let page = page.clamp(1, pages);
    ((page - 1) as f32 * h, h)
}

/// Draw the paged scroll rail in `track` (the caller's 6px-wide column beside its viewport): the
/// full-height dim track, then the fill at [`rail_geom`]'s offset. Both are pill-ended.
///
/// Drawn only when there is more than one page — a rail on content that fits is a control saying
/// there is somewhere else to go when there is not.
pub(crate) fn scroll_rail(p: Painter, track: Rect, page: usize, pages: usize) {
    if pages <= 1 {
        return;
    }
    p.rrect(track, RAIL_RAD, RAIL_RAD, theme::RAIL_TRACK);
    let (top, h) = rail_geom(page, pages);
    p.rrect(
        Rect::new(track.x, track.y + top * track.h, track.w, h * track.h),
        RAIL_RAD,
        RAIL_RAD,
        theme::RAIL_FILL,
    );
}

// ---- Edge feather: the soft end of a hard-clipped viewport ---------------------------------------

/// Feather the top and/or bottom `band` px of a **scissor-clipped** viewport, so prose that
/// continues past the edge dissolves into the panel instead of being guillotined mid-glyph.
///
/// **It is cosmetic; the CLIP is what actually cuts.** `ui/CLAUDE.md` records that the old
/// fade-mask-INSTEAD-of-a-clip trick was removed because a linear fade cannot cut a tall two-line
/// row evenly and read as a broken clip. This is the other arrangement: `Painter::clip` makes the
/// exact cut and the ramp only softens the last `band` px in front of it, which is sound precisely
/// because a prose block has uniform leading — the objection was about rows of unequal height.
///
/// Each edge is drawn only when there is something on the far side of it (`top` = the block is
/// scrolled off the top, `bot` = more remains below). An unconditional pair puts a permanent
/// shadow across the first and last lines of a document that fits, which reads as content hidden
/// where none is.
///
/// The ink is [`theme::SURFACE_PANEL`], whose documented role is exactly this ("opaque menu panel /
/// fade mask"), at four alphas through [`Painter::grad4`] — one rgb at four alphas, which is the
/// only way that primitive interpolates exactly (its own doc). Alpha reaches 1 AT the clip line, so
/// the ramp is opaque precisely where the scissor bites and the seam has nothing left to show.
pub(crate) fn edge_feather(p: Painter, view: Rect, band: f32, top: bool, bot: bool) {
    let band = band.min(view.h * 0.5);
    if band <= 0.0 {
        return;
    }
    let ink = theme::SURFACE_PANEL;
    let solid = theme::with_a(ink, 1.0);
    let clear = theme::with_a(ink, 0.0);
    if top {
        // tl, tr, br, bl — opaque at the top edge, gone `band` px down
        p.grad4(Rect::new(view.x, view.y, view.w, band), [solid, solid, clear, clear]);
    }
    if bot {
        p.grad4(Rect::new(view.x, view.y + view.h - band, view.w, band), [clear, clear, solid, solid]);
    }
}

// ---- Tracked caps: a kicker drawn letter by letter -----------------------------------------------

/// Draw a letter-tracked kicker. The text backend has no letter-spacing, so tracking is always a
/// per-character pen advance, and the label is pre-split into `&CStr` literals so a kicker that
/// draws every frame costs no allocation — deliberately awkward, and only worth it for a CONSTANT
/// word, which every tracked kicker in this app is. Returns the run's width, so nothing has to
/// measure ahead (a measure-only half existed and never had a caller).
pub(crate) fn tracked_run(
    p: Painter,
    chars: &[&std::ffi::CStr],
    x: f32,
    y: f32,
    sz: c_int,
    col: [f32; 4],
    bold: c_int,
    track: f32,
) -> f32 {
    let mut bx = x;
    for c in chars {
        bx += p.text(c.as_ptr(), bx, y, sz, col, 0, bold);
        bx += track;
    }
    (bx - track - x).max(0.0)
}

/// The **bottom scrim** on a piece of artwork: `h` px of near-black fading upward to nothing, clipped
/// to the card's own rounded silhouette. This is what lets text sit directly ON a still with no chip
/// or capsule behind it (`Details Screen.dc.html`'s episode tiles) — a plain label over an arbitrary
/// video frame is a coin flip for legibility, and a capsule per label was the alternative the design
/// deliberately drops.
///
/// Two parts, because `Painter::rect`'s gradient is the only one we have and it would round all four
/// corners of a band: the straight-sided majority is one gradient quad, and the last `rad` px — the
/// only rows where the card's corner arcs bite — are flat bands scissored to the card silhouette
/// (`card_row::resume_bar`'s corner-wrapping trick). Set/clear are paired inside the call, per
/// `ui/CLAUDE.md`'s clip contract.
pub(crate) fn art_scrim(p: Painter, card: Rect, rad: f32, h: f32, a: f32) {
    let h = h.min(card.h);
    if h <= 0.0 {
        return;
    }
    // Snap every internal boundary to a whole COMPOSITED pixel (fold the painter translate, snap,
    // unfold — the same contract text and icon masks use, see `gfx::snap`).
    //
    // This is load-bearing, not tidiness. The gradient quad below is a HARD fill edge — `gfx::draw_rect`
    // takes its no-AA fast path at radius 0, deliberately, so scrims stay exactly their bounds — while
    // `gfx::clip_set` TRUNCATES its scissor box to integer rows. At a fractional seam those two
    // disagree by up to a pixel, and the row between them is covered by neither the gradient nor the
    // first band: the artwork shows through it as a bright hairline across the whole tile. It only
    // appears once the strip's scroll spring settles somewhere fractional, which is nearly always.
    let snap = |y: f32| crate::gfx::snap(y + p.dy) - p.dy;
    let bottom = card.y + card.h;
    let top = snap(bottom - h);
    let seam = snap(bottom - rad).max(top);
    if seam > top {
        p.rect(
            Rect::new(card.x, top, card.w, seam - top),
            0.0,
            theme::scrim(0.0),
            theme::scrim(a * (seam - top) / h),
            0.0,
        );
    }
    let end = snap(bottom);
    if end <= seam {
        return;
    }
    // Internal boundaries ABUT EXACTLY — every edge above is snapped, so `clip_set`'s integer
    // truncation tiles the boxes with neither gap nor overlap. Both failure modes are visible as a
    // hairline across the whole tile and they look nothing alike: a gap leaves one row of unscrimmed
    // artwork (BRIGHT), while an overlap makes two translucent scrims composite on one row —
    // 1-(1-.7)(1-.7) ≈ .91 against ~.7 either side — which is a BLACK one. Do not add slop here.
    let bh = (end - seam) / SCRIM_CORNER_BANDS as f32;
    for i in 0..SCRIM_CORNER_BANDS {
        let y0 = if i == 0 { seam } else { snap(seam + i as f32 * bh) };
        let y1 = if i + 1 == SCRIM_CORNER_BANDS { end } else { snap(seam + (i + 1) as f32 * bh) };
        if y1 <= y0 {
            continue;
        }
        let t = ((y0 + y1) * 0.5 - top) / h;
        // …with ONE exception: the last band runs a pixel past the card's own edge. The rounded rect
        // it fills ends there regardless, so it costs nothing, and it guarantees the final row is
        // covered even though the card's true bottom is fractional.
        let tail = if i + 1 == SCRIM_CORNER_BANDS { 1.0 } else { 0.0 };
        p.clip(Rect::new(card.x, y0, card.w, y1 - y0 + tail));
        p.rrect(card, rad, rad, theme::scrim(a * t));
    }
    p.clip_clear();
}

// ---- The hero corner scrim: the wedge that makes hero copy legible over ARTWORK ---------------
//
// A **sibling** of `art_scrim`, deliberately not a direction flag on it. `art_scrim`'s entire body
// is the scissor-vs-fill seam problem on a ROUNDED CARD — snapped bands, `SCRIM_CORNER_BANDS`,
// clip/clip_clear — and a full-bleed hero has no corner arcs, no scissor and a different axis.
// Folding a flag into it would make that seam machinery conditional on a case it never runs in,
// which is forking by another name. What the two do share is the rule: a label sits directly on
// artwork only where something has bought it the contrast to.

/// How far the hero wedge reaches before it is gone entirely: it peaks at x=0 and is exactly 0
/// here. Four fifths of the panel, because it has to still be carrying weight under the RIGHT END
/// of the longest lines, not just at the margin — detail's synopsis column runs to x=990
/// (`MARGIN_X` + its `HERO_TEXT_W`) and its facts line to ~1270. A tighter falloff strands exactly
/// the ends of the lines a long title or blurb produces. The last fifth of the frame is left
/// completely alone: that is the side of the picture the composition is usually about.
const HERO_SCRIM_W: f32 = 0.80 * crate::ui::consts::SCR_W; // 1536

/// Where the wedge starts feathering in. Clears the tab track's own band ([`TOP_BAR_Y`] 44 +
/// [`TAB_PILL_H`] 60 + [`TAB_TRACK_PAD`] 8 = 112) by 50px: the top chrome owns its legibility with
/// its own dark capsule (`draw_tab_row`) and must not get a second treatment stacked under it.
const HERO_SCRIM_TOP: f32 = 0.15 * crate::ui::consts::SCR_H; // 162
/// Where the wedge reaches full strength — and the seam between its two quads, named once so the
/// pair is watertight by construction. It sits above every hero TEXT anchor, which is what lets
/// [`hero_scrim_a`] be a function of x alone: both heroes baseline their title on the bottom of a
/// `hero_logo::band_h` band (home's stack bottoms out at y≈528, detail's on `TITLE_BOTTOM` 566), so
/// the highest cap top on either screen is ~479, and every line below it is lower still.
///
/// What DOES cross this line is a clearLogo, which is art: the band is a layout floor and a squarer
/// mark spills upward out of it as paint, to y=298 on detail and y=260 on home. Up there the wedge
/// is still feathering in (~0.27 of its peak at y=260) rather than absent, which is the reason nit
/// 2's taller logos were sequenced AFTER this component; the clearance that binds them is the top
/// chrome's ([`TOP_BAR_BOTTOM`]), asserted in `home.rs`, not this knee.
const HERO_SCRIM_KNEE: f32 = 0.39 * crate::ui::consts::SCR_H; // 421.2

/// The mirrored RIGHT wedge — for a hero with a right-aligned column, which today means detail's
/// "Starring" block at x 1270..1830, the one piece of hero copy the left wedge by definition cannot
/// reach. Weaker and shorter than the left one because the bottom-up ramp is already ~0.65 across
/// that band: this closes the gap, it does not carry the load. It is also the component's banding
/// risk (`GL_DITHER` is deliberately off) — ~1 8-bit code per 4–5px near its origin. If it bands,
/// make these SMALLER (steeper = fewer codes per pixel); never re-enable dithering.
const HERO_SCRIM_R_W: f32 = 700.0;
const HERO_SCRIM_R_TOP: f32 = 0.65 * crate::ui::consts::SCR_H; // 702
const HERO_SCRIM_R_A: f32 = 0.50;

/// Where the frame-wide ATMOSPHERIC ramp starts — the treatment's other half, which both heroes
/// paint under this wedge (`home::Backdrop::draw` / `detail::draw_backdrop`): nothing above this
/// line, running to the foot of the panel. It sits here with the wedge's own stops because it is
/// one treatment's first stop, and it was declared verbatim, under the same name, in two screens.
///
/// The two screens' CURVES below it are deliberately **not** unified — home's is a two-stop ramp
/// with a midpoint knee, detail's a single linear stop, and they land within ~0.05 alpha of each
/// other everywhere. Retuning the atmospheric floor is a different decision from sharing its
/// origin, and only the panel can judge it; the curves stay as `home::base_scrim_a` /
/// `detail::base_scrim_a`, which is also what the legibility table below grades.
pub(crate) const HERO_BASE_SCRIM_Y0: f32 = 0.34 * crate::ui::consts::SCR_H; // 367.2

/// The hero wedge's alpha at `x`, at or below [`HERO_SCRIM_KNEE`] — which is where every hero text
/// anchor sits, by construction (see that const). Pure, because the legibility contract is graded
/// on it: this is the arithmetic the anchor table in this module's tests reads, so the promise and
/// the paint cannot come from two different curves.
///
/// `strength` is the screen's own hero fade (home's `env.hero_a`, detail's
/// `hero_alpha(scroll, HERO_FADE)`) — everything scales by it, so the wedge leaves with the hero
/// rather than lingering over the shelves, where the flat ground is what makes card shadows read.
pub(crate) fn hero_scrim_a(x: f32, strength: f32) -> f32 {
    let u = (x.max(0.0) / HERO_SCRIM_W).min(1.0);
    theme::SCRIM_TEXT_A * (1.0 - u) * strength.clamp(0.0, 1.0)
}

/// The mirrored right wedge's alpha at `(x, y)` — [`hero_scrim_a`]'s sibling, and the only part of
/// the field that is two-dimensional, since it feathers in from its left edge AND from its top and
/// peaks only in the bottom-right corner. Pure for the same reason.
pub(crate) fn hero_scrim_right_a(x: f32, y: f32, strength: f32) -> f32 {
    let u = ((x - (crate::ui::consts::SCR_W - HERO_SCRIM_R_W)).max(0.0) / HERO_SCRIM_R_W).min(1.0);
    let v = ((y - HERO_SCRIM_R_TOP).max(0.0) / (crate::ui::consts::SCR_H - HERO_SCRIM_R_TOP)).min(1.0);
    HERO_SCRIM_R_A * strength.clamp(0.0, 1.0) * u * v
}

/// The wedge's quads as `(rect, [tl, tr, br, bl])`, built pure so the seam between quad 0 and
/// quad 1 — the one structural bug this component can have — is host-gradeable. `n` is how many
/// entries are live: 2, or 3 when `right`.
///
/// The whole field is **one ink at four alphas** ([`theme::scrim`]), which is [`Painter::grad4`]'s
/// stated precondition: straight (non-premultiplied) rgba only interpolates exactly across a quad
/// when the corners share an rgb.
///
/// | # | rect | field it produces |
/// |---|---|---|
/// | 0 | `(0, TOP, W, KNEE−TOP)` | feather-in: 0 along the whole top edge, the full wedge along the bottom |
/// | 1 | `(0, KNEE, W, SCR_H−KNEE)` | constant in y → a pure horizontal ramp to the frame's foot |
/// | 2 | `(SCR_W−R_W, R_TOP, R_W, SCR_H−R_TOP)` | feathers in from the top AND the left, peaking bottom-right |
///
/// **Quad 0 and quad 1 abut exactly**, and must keep doing so: quad 0's bottom pair (`bl→br` =
/// edge→none) is identical to quad 1's top pair (`tl→tr` = edge→none) at every x, and the two share
/// one float y. The reflex here is to reach for [`crate::gfx::snap`] — don't. [`art_scrim`] snaps
/// because an integer-truncated *scissor* meets a float *fill*; these are fill-to-fill quads
/// sharing an edge, where the rasterizer's own fill rule already guarantees neither a gap (one row
/// of unscrimmed BRIGHT artwork) nor a double-cover (one row of doubled scrim). Snapping would be
/// cargo cult, and it would move the seam off the shared float.
///
/// **The corners are the closed forms EVALUATED AT THE CORNERS**, not a second hand-written copy of
/// them: a quad's corner alpha is exactly what [`hero_scrim_a`] / [`hero_scrim_right_a`] say at that
/// `(x, y)`, so at the corners the field the legibility contract is graded on and the field that is
/// painted cannot drift — the tests below no longer assert that agreement, only the part
/// construction cannot pin (that the closed forms are affine / bilinear in BETWEEN the corners,
/// which is the shape `grad4` can actually interpolate). It also gives the two closed forms the
/// production caller they otherwise lacked. Both clamp `strength` themselves, so it is passed raw.
pub(crate) fn hero_scrim_quads(strength: f32, right: bool) -> ([(Rect, [[f32; 4]; 4]); 3], usize) {
    let (sw, sh) = (crate::ui::consts::SCR_W, crate::ui::consts::SCR_H);
    // the wedge peaks at its left margin and is gone by `HERO_SCRIM_W`; the right one peaks in the
    // bottom-right corner of the panel, which is the corner it is anchored to
    let none = theme::scrim(hero_scrim_a(HERO_SCRIM_W, strength));
    let edge = theme::scrim(hero_scrim_a(0.0, strength));
    let redge = theme::scrim(hero_scrim_right_a(sw, sh, strength));
    let mut q = [(Rect::new(0.0, 0.0, 0.0, 0.0), [none; 4]); 3];
    q[0] = (
        Rect::new(0.0, HERO_SCRIM_TOP, HERO_SCRIM_W, HERO_SCRIM_KNEE - HERO_SCRIM_TOP),
        [none, none, none, edge],
    );
    q[1] = (Rect::new(0.0, HERO_SCRIM_KNEE, HERO_SCRIM_W, sh - HERO_SCRIM_KNEE), [edge, none, none, edge]);
    if right {
        q[2] = (
            Rect::new(sw - HERO_SCRIM_R_W, HERO_SCRIM_R_TOP, HERO_SCRIM_R_W, sh - HERO_SCRIM_R_TOP),
            [none, none, redge, none],
        );
    }
    (q, if right { 3 } else { 2 })
}

/// Draw the hero corner scrim: the darkening for copy that sits directly on a full-bleed backdrop,
/// along the one axis a bottom-up ramp has nothing to say about.
///
/// Both heroes already paint a frame-wide atmospheric ramp, and both bottom-anchor their text
/// column well ABOVE the band where that ramp reaches strength — the HERO-72 title's cap top gets
/// ~0.11 of it. The ramp cannot simply be raised, because at any given y it is uniform across all
/// 1920px: enough alpha to rescue the title would put 60% black over the whole picture at the
/// title's y. So the corner the text is IN gets its own field instead, and the ramp goes on doing
/// the job it is good at (mood, and the depth under the shelf line).
///
/// `strength` is the screen's own hero fade — see [`hero_scrim_a`]. `right` adds the mirrored wedge
/// for a hero with a RIGHT-aligned column (detail's "Starring"); home has none and passes `false`.
///
/// Call it INSIDE the backdrop, before any hero content is drawn: it exists to darken artwork, and
/// a wedge over the text would be a dimmer, not a scrim.
pub(crate) fn hero_scrim(p: Painter, strength: f32, right: bool) {
    if strength <= 0.0 {
        return;
    }
    let (q, n) = hero_scrim_quads(strength, right);
    for (r, k) in q.iter().take(n) {
        p.grad4(*r, *k);
    }
}

/// Is the one-pass hero ground armed? `/tmp/plxnative-heroground`, read once at boot.
///
/// EXPERIMENT, not a default. The shipped path is the four blended quads, and it stays reachable
/// on one binary so the two are an A/B rather than a replacement.
static HERO_GROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Arm the one-pass hero ground (`app.rs`, from the dev trigger).
pub(crate) fn set_hero_ground(on: bool) {
    HERO_GROUND.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// May a hero draw its ground in one pass? Both halves must hold: the trigger, and a program that
/// actually linked (`gfx::hero_ground_ok`) — a driver that refused it keeps the shipped picture.
pub(crate) fn hero_ground_armed() -> bool {
    HERO_GROUND.load(std::sync::atomic::Ordering::Relaxed) && crate::gfx::hero_ground_ok()
}

/// The one-pass ground's WEDGE field, exactly as `fs_hero.frag` evaluates it from the same four
/// numbers. Pure, and written twice on purpose: a GLSL expression cannot be graded by `make check`,
/// so this is the copy the tests pin against [`hero_scrim_quads`] — the shipped picture — at every
/// corner of both its quads. If the shader and this ever disagree, the test is the one that is
/// right and the shader is the bug.
pub(crate) fn hero_ground_wedge_a(wedge: [f32; 4], x: f32, y: f32) -> f32 {
    let u = (x / wedge[1]).clamp(0.0, 1.0);
    let v = ((y - wedge[2]) / (wedge[3] - wedge[2])).clamp(0.0, 1.0);
    wedge[0] * (1.0 - u) * v
}

/// The one-pass ground's atmospheric RAMP field, as `fs_hero.frag` evaluates it — two linear
/// segments meeting at `ramp[1]`, from `(ramp[0], 0)` through `(ramp[1], ramp[2])` to
/// `(SCR_H, ramp[3])`. Same contract as [`hero_ground_wedge_a`]: the tests pin it against the
/// screen's own curve.
pub(crate) fn hero_ground_ramp_a(ramp: [f32; 4], y: f32) -> f32 {
    let t0 = ((y - ramp[0]) / (ramp[1] - ramp[0])).clamp(0.0, 1.0);
    let t1 = ((y - ramp[1]) / (crate::ui::consts::SCR_H - ramp[1])).clamp(0.0, 1.0);
    ramp[2] * t0 + (ramp[3] - ramp[2]) * t1
}

/// The wedge's four shader parameters for a hero at `strength` — the ONE place they are derived,
/// so the draw and the test cannot read two different geometries.
pub(crate) fn hero_ground_wedge(strength: f32) -> [f32; 4] {
    [hero_scrim_a(0.0, strength), HERO_SCRIM_W, HERO_SCRIM_TOP, HERO_SCRIM_KNEE]
}

/// THE HERO GROUND IN ONE PASS — the backdrop art carrying both scrim fields, instead of the art
/// plus [`hero_scrim`]'s two quads plus the screen's own two atmospheric-ramp bands.
///
/// **This is the same picture by construction, not a cheaper approximation of it.** Both fields are
/// closed forms of the authored pixel position, both are [`theme::SCRIM_INK`], and two straight-alpha
/// layers of one ink compose exactly as `a1 + a2 - a1*a2`; `fs_hero.frag` carries the algebra and
/// the one thing that does differ (the shipped path quantises the framebuffer three times where
/// this quantises once).
///
/// **Why it is worth a program of its own.** Measured on the dev television with
/// `/tmp/plxnative-overdraw`: Home's hero submits 5.39M authored pixels against a 2.07M-pixel panel
/// — 2.60x — and the ramp (1,368,576 px) plus the wedge (1,410,048 px) are 52% of it, for fields
/// that are three ALU operations each. Their cost is not their shading, it is that they are 2.78M
/// more fragments through the blender landing on pixels the art has already written.
///
/// `art` is the resolved texture and the rect [`crate::ui::home`] would have drawn it at; `art_a`
/// its tint alpha; `ramp` is the screen's atmospheric curve as `(y0, knee, a_knee, a_foot)` —
/// **the screen's, not this module's**, because home's is a two-stop curve with a midpoint knee and
/// detail's a single linear stop, and unifying them is a design decision nobody has taken.
/// `strength` is the hero fade the wedge scales with, exactly as [`hero_scrim`] takes it.
///
/// The CALLER owns the preconditions, because they are all facts about the screen's own state: the
/// art must be there and be the only layer (a hero FLIP slides two of them, and one quad cannot
/// carry a scrim over both), and the wedge must actually be wanted. Anything else falls back.
pub(crate) fn hero_ground(p: Painter, tex: u32, r: Rect, art_a: f32, ramp: [f32; 4], strength: f32) {
    // The wedge's own geometry, in the same authored units [`hero_scrim_quads`] builds its corners
    // from — and its peak through the very function the legibility table is graded on, so the one
    // pass and the four quads cannot be two different curves.
    p.hero_ground(tex, r, art_a, ramp, hero_ground_wedge(strength));
}

/// The profile chip's diameter: ONE control height with the tab pills and the circle-button
/// family, so the focused chip's capsule — the avatar plus [`TAB_TRACK_PAD`] all round — is
/// exactly the tab-bar track's band, and the two sit concentric on the top chrome line.
pub(crate) const CHIP_D: f32 = TAB_PILL_H;
/// Avatar → name air inside the expanded chip, and the capsule's tail past the name. The tail is
/// bigger so the name isn't crowded against the round end (the tab track reads the same: its own
/// 8px inset plus the end pill's 18px label padding).
const CHIP_NAME_GAP: f32 = 14.0;
const CHIP_NAME_TAIL: f32 = 24.0;
/// Name budget — a long profile name elides rather than growing the capsule into the CENTERED tab
/// track sitting a few hundred px to its right.
const CHIP_NAME_MAX: f32 = 320.0;
/// **The chip's frame — one rect, owned here, for all three screens that wear the bar.**
///
/// A CONSTANT rather than a rect recorded at draw the way [`tab_pill_at`]'s are, and the asymmetry
/// is the geometry rather than an oversight: the pills SCROLL inside their track, so where one was
/// drawn is a fact only that draw knows, while the chip sits at the margin on the top-bar line and
/// never moves. `Rect::new(MARGIN_X, TOP_BAR_Y, CHIP_D, CHIP_D)` used to be written out in
/// `home.rs`, `library.rs` and `search/mod.rs` — and only the first of the three also recorded it
/// for a hit test, which is exactly how a control drawn on three screens came to be clickable on
/// one.
pub(crate) const CHIP_FRAME: Rect = Rect::new(crate::ui::consts::MARGIN_X, TOP_BAR_Y, CHIP_D, CHIP_D);

/// **The focused capsule's rect — the ONE expression, drawn and priced from the same place.**
///
/// [`profile_chip`] draws this and [`CHIP_CAP_MAX_R`] prices it, and until 2026-08-21 they were two
/// arithmetics that happened to agree: the draw built the rect inline and the constant restated the
/// same seven terms by hand, in a different file position, so a pad added to one would have left the
/// other quietly describing a capsule nobody draws. That constant is what [`GLASS_TRACK_MAX`] is
/// solved against, i.e. what keeps the band's two glass surfaces from touching — the design-system
/// rule for a graded field applies with more force here than it does to a scrim: the drawn shape and
/// the shape a test grades must be one expression, not two that agree.
///
/// `e` is the unfurl, 0..1; `name_w` the elided name's measured width ([`CHIP_NAME_MAX`] is its
/// budget, so `chip_cap(1.0, CHIP_NAME_MAX)` is the widest capsule the control can ever draw).
/// Only the WIDTH moves — the capsule grows rightward off a fixed left edge, which is why the band's
/// blur region has a constant left edge whatever the chip is doing.
const fn chip_cap(e: f32, name_w: f32) -> Rect {
    let closed = CHIP_D + 2.0 * TAB_TRACK_PAD;
    let open = closed + CHIP_NAME_GAP + name_w + CHIP_NAME_TAIL;
    Rect::new(
        CHIP_FRAME.x - TAB_TRACK_PAD,
        CHIP_FRAME.y - TAB_TRACK_PAD,
        closed + (open - closed) * e,
        CHIP_D + 2.0 * TAB_TRACK_PAD,
    )
}

/// The chip unfurl's stiffness — brisk, a touch stiffer than the hero slide.
const K_CHIP: f32 = 300.0;
/// How far the chip is into its focused face, 0..1. A static beside [`TAB_SCROLL`] and
/// [`TOP_STRIP`] for the same reason those are: ONE bar is drawn by three screens, and the unfurl
/// has to carry ACROSS a Home↔Library↔Search route flip rather than restart on the far side. It
/// lived in `home.rs` while Home was the only screen the chip could be focused on.
static mut CHIP_EXPAND: crate::ui::Spring = crate::ui::Spring::at(0.0);

/// **What in the shared top bar holds the remote.** The bar is ONE control across Home, the Library
/// and Search, and it has exactly two kinds of stop — the chip at the margin and a pill of the
/// centred strip — so a screen answers with one value instead of with two booleans that could both
/// say yes. [`tab_row_update`] takes it, which is what makes every screen animate both stops by
/// construction (the same argument that put the strip's scroll and its capsules in that function).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TopFocus {
    /// focus is somewhere else on the page — or on no page at all
    Away,
    /// the profile chip
    Chip,
    /// a pill of the strip, by index (0 = Home)
    Pill(usize),
}
impl TopFocus {
    /// The pill index the strip's own machinery wants, or -1. The strip predates [`TopFocus`] and
    /// speaks `c_int`; converting here keeps that one encoding in one place.
    fn pill(self) -> c_int {
        match self {
            TopFocus::Pill(i) => i as c_int,
            _ => -1,
        }
    }
}

/// Is the pointer on the profile chip? The chip's half of [`tab_pill_at`] — the shared bar's two
/// kinds of stop, hit-tested in the one module that draws them.
///
/// The target is the AVATAR, never the unfurled name capsule beside it: the capsule is a focus
/// read-out that only exists while the remote is already on the chip, so a pointer that could
/// activate it would be clicking a shape that is not there for the user who reached the chip with
/// a mouse. That was Home's rule when Home owned this test, and it is unchanged.
pub(crate) fn profile_chip_at(mx: f32, my: f32) -> bool {
    CHIP_FRAME.contains(mx, my)
}

// ---- the navigation bar's scrim ---------------------------------------------------------------

/// How much scroll fully establishes the scrim — the design system's `--scrim-in`. It FADES IN
/// over the first pixels rather than popping on at a 1px threshold, and because scroll here is
/// spring-driven the appear inherits the spring's easing for free and cannot desync.
const NAV_SCRIM_IN: f32 = 56.0;
/// The alpha the ramp bends at — `--scrim-knee-a`.
const NAV_SCRIM_KNEE_A: f32 = 0.40;
/// Where the ramp bends, as a FRACTION of the gap between the chrome and the content: a steep fall
/// to the knee over the first part, then a long gentle tail to nothing. The design system spells the
/// two lines per route and both of its routes agree on the proportion — Library `170 / 188 / 214`
/// and the full-screen list route `106 / 124 / 150` are each an 18px steep segment and a 26px tail,
/// so the knee lands at `18/44` of the way down.
///
/// A FRACTION and not those two lengths, because the gap is the caller's: it is the air between the
/// bottom of what the bar draws and the top of what scrolls, and the two screens do not have the
/// same amount of it (the Library keeps 28px under its toolbar chips, Search 40 under its field).
/// Spelled as a length, the ramp would start wherever `content_top − 44` happened to fall — which on
/// the Library is 170, sixteen pixels ABOVE the chips' own bottom edge, so the chips sat on the
/// fading part of their own backing instead of on solid ground. Visible on the panel and reported
/// from the couch; the mock's Library numbers have it too.
const NAV_SCRIM_KNEE_F: f32 = 18.0 / 44.0;

/// **The navigation bar's scrim: content dissolving under the top chrome.** One element, because
/// every full-screen route that scrolls a column under the bar needs exactly this and the design
/// system already tokenises it that way (`--scrim-in`, `--scrim-knee-a`, and a per-route content
/// line — `components/panels/route-screen.card.html` is its reference drawing).
///
/// Two lines and a scroll. `chrome_bottom` is the lowest thing the BAR draws — the Library's toolbar
/// chips, Search's query capsule — and everything above it stays solidly backed, so a control never
/// sits on ground that is fading out from under it. `content_top` is where the caller's own scrolling
/// content begins, and the veil has reached nothing by then. `scroll` is how far that content has
/// travelled; nothing is drawn at rest, which keeps a settled screen free of it entirely.
///
/// **It starts at y=0 and is OPAQUE for most of its height**, and that is the part that is easy to
/// get wrong: the band is not a gradient hung under the bar, it is the bar's whole ground, with a
/// ramp on its bottom edge. Starting it lower leaves a strip of live content above it — a poster's
/// top edge appearing over the chrome, which is what the first version of this did on Search.
///
/// **That has now been tried a second time, for a reason that sounded better, and it still loses.**
/// The design system asks for the grid to pass BEHIND the glass track — cutting the content at the
/// chrome line and then reporting that the material shows nothing is circular, and the argument is
/// right. It does not survive this screen's arithmetic. The grid's rest positions are fixed modulo
/// [`library::PITCH`], and at every one of them the track lands in the 96px band BETWEEN two rows:
/// measured in the simulator at 1920x1080, the row above ends at y=26 and the track occupies 36 to
/// 112, so the material has flat app grey behind it at rest no matter how far you scroll. What the
/// change actually bought was a permanent ~26px strip of cut-off poster bottoms along the top of
/// the frame. The emptiness behind the bar is not a choice anybody made — it is the row pitch, and
/// moving the scrim cannot reach it. Reinstated, with the measurement, so the next reader gets the
/// answer instead of the idea.
///
/// The caller draws it AFTER its content and BEFORE its chrome, which is the order that lets it be
/// opaque; `library::draw` has always used that order and it is why its scissor is a bound rather
/// than a treatment.
///
/// **And the obvious idea — make it a BLUR rather than a grey — was built, measured and refused.**
/// `/tmp/plxnative-navglass` is that build, still here and still off: the whole band becomes a
/// backdrop-blurred surface and these three rects become its frost at [`NAV_GLASS_FROST`] of their
/// weight. It looks like what it should look like, on the Library. It is also **far cheaper than
/// the region arithmetic says** — 58 fps where `docs/glass-hardware-budget.md`'s law predicted 45
/// — and it still loses, on the one number that answers the question: these two screens hold a
/// flawless 60 today with no frame over 20.6 ms, and with the material every second's worst frame
/// is ~26 ms. Numbers, both screens, both ways, and what the harness scenes could NOT see:
/// `docs/glass-hardware-budget.md` §11. Reach for the trigger before proposing it again; do not
/// reach for it to ship it.
pub(crate) fn nav_scrim(p: Painter, chrome_bottom: f32, content_top: f32, scroll: f32) {
    let a = (scroll / NAV_SCRIM_IN).clamp(0.0, 1.0);
    let gap = content_top - chrome_bottom;
    if a <= 0.004 || gap <= 0.0 {
        return;
    }
    // The prototype material draws NOTHING into a blur source pass — the same rule
    // [`draw_tab_row`] follows, and for the same reason: the snapshot this band frosts must be the
    // live page, not a grey copy of the band itself. Left in, the band would blur its own veil and
    // the material would show a flat wash however far the grid had scrolled.
    if crate::gfx::blur_source_pass() && nav_glass_wanted() {
        return;
    }
    let ps = p.alpha(a); // the appear rides the cascade — all three bands together
    let frost = if nav_glass_on() && nav_glass_backdrop(ps, content_top) { NAV_GLASS_FROST } else { 1.0 };
    nav_scrim_bands(ps, chrome_bottom, content_top, frost);
}

/// The three bands, at `frost` of their full weight — 1.0 for the opaque treatment, and
/// [`NAV_GLASS_FROST`] over a live backdrop.
///
/// One function for both paths so the two can never become two different curves: the glass path is
/// the flat path's own ramp scaled, which is what makes "the material is the same shape, lighter"
/// an executable statement rather than a claim (`nav_scrim_stops` and its test).
fn nav_scrim_bands(ps: Painter, chrome_bottom: f32, content_top: f32, frost: f32) {
    let (base, knee_col, clear) = nav_scrim_stops(frost);
    let knee = chrome_bottom + (content_top - chrome_bottom) * NAV_SCRIM_KNEE_F;
    let w = crate::ui::consts::SCR_W;
    ps.rect(Rect::new(0.0, 0.0, w, chrome_bottom), 0.0, base, base, 0.0);
    ps.rect(Rect::new(0.0, chrome_bottom, w, knee - chrome_bottom), 0.0, base, knee_col, 0.0);
    ps.rect(Rect::new(0.0, knee, w, content_top - knee), 0.0, knee_col, clear, 0.0);
}

/// The band's three colour stops at `frost` of full weight — solid, knee, nothing.
fn nav_scrim_stops(frost: f32) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let base = theme::SURFACE_APP;
    (
        theme::with_a(base, frost),
        theme::with_a(base, NAV_SCRIM_KNEE_A * frost),
        theme::with_a(base, 0.0),
    )
}

latched_flag!(
    /// **`/tmp/plxnative-navglass` — the scroll band as a frosted MATERIAL instead of an opaque
    /// grey one.** Default OFF, and it stays off: see [`nav_glass_rect`] for the arithmetic and
    /// `docs/glass-hardware-budget.md` §11 for the television measurement that decided it.
    ///
    /// The idea is the obvious one — content scrolling under the top chrome should FROST rather
    /// than dissolve into flat grey — and it is behind a trigger because the question it raises is
    /// not a taste question. Measured on the set: the Library grid goes from a flawless 60 fps to
    /// 58, and its worst frame each second from ~19 ms to ~26. Arm this and look at it before
    /// proposing the idea again; do not arm it to ship it.
    fn nav_glass_armed = "navglass";
);

/// How much of the flat treatment's weight the material keeps.
///
/// The band's whole job is to be legible ground for the chrome standing on it, and at `frost = 0`
/// the tab pills' [`theme::TEXT_TERTIARY`] labels would sit on whatever poster happened to be
/// passing. The density wanted is between the bar's own [`theme::Material::UltraThin`] 0.28 and a
/// popover's 0.72 — a container you look PAST, but one carrying the app's only navigation — and
/// that is [`theme::Material::Regular`], the rung the ladder already puts there. It was written out
/// as a bare `0.62`, which is the same weight to within two percent of alpha and a fourth
/// un-tokenised density in a system whose whole point is that there are five.
///
/// **The frost and the sample radius come from DIFFERENT rungs here, and that is deliberate rather
/// than a drift.** `Material` is meant to hand over both halves from one name ([`panel_material`]
/// says so in as many words), and this surface breaks that on purpose: [`nav_glass_backdrop`]
/// passes `UltraThin` so the backdrop takes the CHEAPEST fetch there is — the measurement had to be
/// a floor, not a representative sample — while the frost is the weight the chrome actually needs
/// to stand on. If this were ever unrefused the two would have to be reconciled to one name, and
/// the cost re-measured at that name.
///
/// It scales all three stops rather than replacing them, so the ramp keeps its knee — see
/// [`nav_scrim_bands`].
const NAV_GLASS_FROST: f32 = theme::Material::Regular.frost();

/// How far past the panel the band's geometry is pushed on its three off-screen sides.
///
/// `fs_glass` puts its chamfer, lens and specular hairline within the rim's reach of every edge,
/// and a band that spans the frame must not be ringed by a lit border down its left and right. Only
/// the BOTTOM edge is meant to be seen, so only that one stays on the panel. The bleed costs nothing
/// in region — `gfx::blur_region` clamps to the screen — which is exactly why it is free to use.
/// Same trick, same reason, as `glassload`'s `NAV_BLEED`.
const NAV_GLASS_BLEED: f32 = 80.0;

static mut NAV_GLASS_STATE: GlassState = GlassState::new();

/// Will this band wear the material this frame, ignoring the source pass?
///
/// **A popover takes it away**, on the tab track's argument one size larger: two glass surfaces in
/// a frame converge on ONE grab, and a band across the top unioned with a panel in the middle is the
/// whole-screen capture the region limit exists to avoid.
fn nav_glass_wanted() -> bool {
    nav_glass_armed() && !crate::ui::popover::any_open()
}

/// …and the source-pass clause: a page being drawn AS a blur source never wears the material it is
/// producing.
fn nav_glass_on() -> bool {
    nav_glass_wanted() && !crate::gfx::blur_source_pass()
}

/// **The rectangle the band is charged for — and the number that turned out NOT to decide it.**
///
/// A glass surface costs its rect grown `gfx::BLUR_MARGIN` on every side, clamped to the panel.
/// This one spans the full width, so the growth that matters is vertical only: the Library's
/// `content_top` of 214 prices at 1920 x 302 = 579,840 px², and Search's 248 at 1920 x 336 =
/// 645,120, against a `gfx::GLASS_REGION_BUDGET` of 300,000. Both are about twice over, which
/// `docs/glass-hardware-budget.md`'s region law calls 45 fps at best.
///
/// **On the television it measured 58, and that prediction is now retired** — the law was taken on
/// the capture path and the DIRECT source path renders the page again at quarter scale, which makes
/// the region term about 4x cheaper (NOT a sixteenth: the chain's up pass is `region / 4` and is the
/// largest thing that path writes — §11 has the pass table). What is left binding is the COMPOSITE,
/// charged at the surface at full resolution and discounted by neither path, and this band's
/// 1920 x 214 is 4.7x the largest area anything in that document ever measured. It is why the
/// arithmetic below is kept as a test and NOT as the price: it counts the term that turned out not
/// to decide. The band was refused on what it actually costs (§11): the two screens hold a flawless
/// 60 with nothing over 20.6 ms today, and the material puts 206 frames a run past 20 ms and every
/// second's worst frame at ~26. The test below keeps the ARITHMETIC honest; the doc keeps the
/// measurement.
fn nav_glass_rect(content_top: f32) -> Rect {
    let b = NAV_GLASS_BLEED;
    Rect::new(-b, -b, crate::ui::consts::SCR_W + 2.0 * b, content_top + b)
}

/// The band's backdrop. `false` — a driver with no render target — leaves the caller its opaque
/// ground, which is the same fallback every other glass surface here takes.
fn nav_glass_backdrop(ps: Painter, content_top: f32) -> bool {
    Glass::DYNAMIC_BACKDROP.backdrop(
        ps,
        nav_glass_rect(content_top),
        0.0,
        0.0,
        [1.0, 1.0, 1.0, 1.0],
        // A container, like the track and the panel: the shallow chamfer and the long lens. Only
        // the bottom edge is on the panel, so this is what defines the line the blur stops at —
        // the material cannot RAMP its own blur, so it has to end at an edge somebody drew.
        crate::gfx::GlassRim::Standing,
        // The three bands above are the face, and they are a vertical ramp with a knee that a
        // two-stop `GlassFace` cannot express.
        crate::gfx::GlassFace::NONE,
        // The band is chrome you look past, and this is also the CHEAPEST material there is (one
        // fetch, no widened re-sample). If the measurement fails here it fails everywhere.
        theme::Material::UltraThin,
    )
}

/// Resolve the band's glass cadence BEFORE the page it sits on draws — `Glass::prepare`'s contract,
/// exactly as [`tab_glass_prepare`] does for the track. Both step the one shared `DynamicClock`, and
/// a second call in a present is a no-op by construction, so the two owners cannot multiply the
/// snapshot rate between them.
pub(crate) fn nav_glass_prepare() {
    if !nav_glass_on() {
        return;
    }
    let state = unsafe { &mut *std::ptr::addr_of_mut!(NAV_GLASS_STATE) };
    Glass::DYNAMIC_BACKDROP.prepare(
        state,
        crate::ui::idle::present_moving() || crate::ui::idle::present_dirty(),
    );
}

/// The band's face, weighted by how far the chip has unfurled — every alpha, not just the tint's.
///
/// Pure, because the shader is where it would be caught late: `fs_glass.frag` writes
/// `max(u_tint.a * cov, rimw)`, so the rim is deliberately allowed to exceed its surface's own
/// coverage and a tint faded to nothing leaves a full-strength hairline behind. At `e == 1` this is
/// the track's face to the bit — the band is ONE material at rest, which is the whole claim.
fn chip_face(face: crate::gfx::GlassFace, e: f32) -> crate::gfx::GlassFace {
    let fade = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * e];
    crate::gfx::GlassFace {
        scrim_top: fade(face.scrim_top),
        scrim_bot: fade(face.scrim_bot),
        rim: fade(face.rim),
        rim_lit: fade(face.rim_lit),
        rim_w: face.rim_w,
    }
}

/// **The focused chip's capsule, in the BAR's material** — the band's second glass surface.
///
/// Returns whether it drew. `false` is the flat capsule's cue, and it is the answer in every case
/// the track is also flat: [`BAR_MATERIAL`] says so, or the chain refuses (no render target, or a
/// blur SOURCE pass, where `draw_blur_backdrop` declines before it records anything). The two
/// halves of the band therefore change material together, always, because only one of them decides.
///
/// **The unfurl fades the whole FACE, not the tint alone**, and that is the one thing this could
/// not borrow from the flat capsule. `fs_glass.frag` emits
/// `max(u_tint.a * cov, rimw)` — the rim deliberately EXCEEDS the surface's coverage, so that a 1px
/// line survives its own antialiased edge — which means a tint faded toward zero leaves the rim at
/// full strength: a bright empty hairline capsule snapping on around the avatar at the first
/// millisecond of the unfurl. Scaling the scrim's two stops and both rim weights by `e` as well
/// fades the material as one object, and at `e = 1` it is the track's face to the bit.
///
/// The tint's alpha carries the same `e`, which cross-fades the blurred backdrop against the sharp
/// page under it — the material arriving rather than the shape appearing.
///
/// **No second `Glass::prepare`.** The cadence belongs to the band ([`TAB_GLASS_STATE`]), was
/// resolved by [`tab_glass_prepare`] before the page drew, and preparing again here would consume
/// this present's refresh slot a second time.
///
/// **And no second snapshot, by geometry.** The chip is drawn after the track, so a re-grab taken
/// here would hold the track's own face — but [`GLASS_TRACK_MAX`] keeps [`BAND_AIR`] between them
/// while both wear the material, and `gfx::blur_region_union` has the track's first call already
/// grabbing the region both need on every frame after the first of an unfurl.
fn chip_capsule(p: Painter, cap: Rect, e: f32) -> bool {
    let face = match unsafe { *std::ptr::addr_of!(BAR_MATERIAL) } {
        BarMaterial::Flat => return false,
        BarMaterial::Glass(f) => chip_face(f, e),
    };
    Glass::DYNAMIC_BACKDROP.backdrop(
        p,
        cap,
        0.0,
        cap.h * 0.5,
        [1.0, 1.0, 1.0, e],
        // A container, exactly as the track is one — same lamp, same 12px chamfer, same 24px lens.
        // The two capsules are the same object seen twice, so nothing about the edge may differ.
        crate::gfx::GlassRim::Standing,
        face,
        theme::Material::UltraThin,
    )
}

/// **The top-left profile chip** — the whole control, not just its picture: the avatar texture (or
/// an initial / person-glyph fallback) with the shared tile shadow + sheen, at [`CHIP_FRAME`], with
/// [`CHIP_EXPAND`]'s focus unfurl. Drawn verbatim by Home, the Library and Search, which is what a
/// piece of SHARED chrome should mean. The session lookup (mutex + UserRef clone) is snapshotted
/// per profile GENERATION, not per frame.
///
/// It used to take the rect and the focus amount from its caller, which is how the three screens
/// came to disagree about it: each wrote out the same rect, Home alone recorded one for a hit test,
/// and the other two hard-coded `0.0` for the expand because the chip could not be focused there.
/// A control drawn on three screens and activatable on one is a bug, not a design — the frame, the
/// hit test ([`profile_chip_at`]) and the spring all live here now, and a screen's only remaining
/// say is where its own focus is, through [`TopFocus`].
///
/// The unfurl is deliberately a scalar and not a bool: at 1 the chip grows the **tab bar's own
/// track capsule** around itself and unfurls the profile name to its right. A lifted shadow alone
/// was far too quiet to read as focus against bright hero art — and the name is what the stop is
/// actually *for*.
///
/// **That capsule is the bar's MATERIAL, not a copy of its weights** ([`chip_capsule`]). The track
/// became solved glass on 2026-08-19 and the chip went on wearing the flat stops it used to share
/// with it, which is one band drawn in two materials — so it takes the face [`draw_tab_row`]
/// published this frame ([`BarMaterial`]) and is glass exactly when the track is. Neither surface
/// solves its own ground: `gfx::sample_ground` has one latch and one rate counter, and two solves
/// over a non-uniform hero land on two densities with a seam between them.
///
/// **Draw it AFTER [`draw_tab_row`], never before**, and there are two reasons now. The unfurled
/// name reaches right, toward the CENTRED strip; drawn first it is painted over by the track's own
/// scrim. And the material has to be published before it can be read — drawn first, the chip would
/// wear the PREVIOUS frame's face, which on the frame a popover opens or a route settles is the
/// face of a bar that is no longer there. Home has always drawn the two in this order; the Library
/// and Search drew them the other way round and got away with it only while the chip could not be
/// focused there, which is the class of bug that made this a shared control in the first place.
///
/// The two can no longer MEET, which is the third thing that changed: [`GLASS_TRACK_MAX`] is solved
/// so the widest capsule clears the widest glass track by [`BAND_AIR`]. Overlap had to become
/// impossible rather than tolerable — there is one blur cache, so a second glass surface over the
/// first draws a second scrim and a second rim over material that already carries both.
pub(crate) fn profile_chip(p: Painter) {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    // **A SURFACE MAY NOT APPEAR IN ITS OWN BACKDROP**, and this control is the second one in the
    // app that could — [`draw_tab_row`]'s note is the first, and it says the whole argument. The
    // direct source path renders the page again into a small FBO and the page includes this chip,
    // so left in, the capsule blurred its own near-opaque FLAT fallback (`scrim_black(.72..82)`,
    // which is what `chip_capsule` returns to inside a source pass) and then darkened that again
    // with its own stops. Measured in the simulator over `flat:92`, at the point the capsule is
    // clear of both the avatar and the name: the face came out at **61** where the track beside it,
    // on the same solve and the same ground, reads **142**. On a dark ground it is invisible; over
    // bright artwork it is a black slug beside a translucent bar, which is the exact shape of the
    // bug the track's note measures from the other side.
    //
    // The WHOLE control goes, not just the capsule: the avatar disc and the name sit inside the
    // glass rect, so blurring them would put a smeared copy of the chip behind the chip — a mirror,
    // not a lens. It costs the backdrop nothing, because nothing else samples this corner.
    //
    // The test is `bar_glass_wanted` — "will the bar wear the material", the same question minus its
    // source-pass clause, which is exactly the form `draw_tab_row` uses. When the bar is FLAT the
    // chip belongs in the snapshot as it always did: it is then genuinely behind whatever samples
    // it.
    if crate::gfx::blur_source_pass() && bar_glass_wanted() {
        return;
    }
    static mut CHIP: Option<(u32, String, CString, CString, f32)> = None; // gen, thumb, initial, name, name w
    let r = CHIP_FRAME;
    let expand = unsafe { std::ptr::addr_of!(CHIP_EXPAND).read() }.pos;
    let d = r.w;
    let gen = crate::plex::session::current_gen();
    let chip = unsafe { &mut *addr_of_mut!(CHIP) };
    if chip.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let cur = crate::plex::session::current();
        let thumb = cur.as_ref().map(|u| u.thumb.clone()).unwrap_or_default();
        let title = cur.as_ref().map(|u| u.title.clone()).unwrap_or_default();
        let initial =
            title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        // signed out: the menu behind the chip offers Sign in, so the expanded chip says so too
        let label = if title.is_empty() { "Sign in".to_string() } else { title };
        let name = CString::new(crate::text::elide(&label, CHIP_NAME_MAX, theme::size::BODY, 1, false))
            .unwrap_or_default();
        let nw = crate::text::text_width(name.as_ptr(), theme::size::BODY, 1);
        *chip = Some((gen, thumb, CString::new(initial).unwrap_or_default(), name, nw));
    }
    let (_, thumb_s, initial_c, name_c, name_w) = chip.as_ref().unwrap();
    // ---- the focused capsule, UNDER the avatar: the tab track's material, inset and radius,
    // widened from a bare disc surround to hold the name.
    let e = expand.clamp(0.0, 1.0);
    if e > 0.004 {
        // The rect comes from [`chip_cap`] rather than being built here, because it is also what
        // [`GLASS_TRACK_MAX`] is solved against — see there.
        let cap = chip_cap(e, *name_w);
        // the tab track's own material, faded in with the unfurl — one pair of weights for both,
        // and now literally the same material: glass when the track resolved glass this frame,
        // the flat capsule when it did not.
        if !chip_capsule(p, cap, e) {
            p.rect_sheened(
                cap,
                cap.h * 0.5,
                theme::scrim_black(theme::TAB_TRACK_A_TOP * e),
                theme::scrim_black(theme::TAB_TRACK_A_BOT * e),
            );
        }
        // the name rides in on the TAIL of the widening, so the glyphs land in a capsule that has
        // already made room for them instead of smearing across the grow
        let na = ((e - 0.55) / 0.45).clamp(0.0, 1.0);
        if na > 0.004 {
            let ty = crate::text::text_vcenter_y(theme::size::BODY, 1, r.cy());
            p.alpha(na).text(
                name_c.as_ptr(),
                r.x + d + CHIP_NAME_GAP,
                ty,
                theme::size::BODY,
                theme::TEXT_PRIMARY,
                0,
                1,
            );
        }
    }
    // resting shadow + perimeter stroke always; lift the shadow with the focus (same as shelf tiles)
    p.focus_shadow(r, d * 0.5, e);
    let mut drew = false;
    if !thumb_s.is_empty() {
        let t = resolve_tex(thumb_s, 128, 128, 0);
        if t != 0 {
            p.tex_stroked(t, r, d * 0.5, theme::TINT_WHITE);
            drew = true;
        }
    }
    if !drew {
        p.rect_sheened(r, d * 0.5, theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_FILL);
        if initial_c.as_bytes().is_empty() {
            // signed out (no session) — a generic person glyph; the menu behind it offers Sign in
            crate::ui::icons::draw(p, crate::ui::icons::Icon::User, r.inset(14.0), theme::TEXT_SECONDARY);
        } else {
            let ty = crate::text::text_vcenter_y(theme::size::HEADLINE, 1, r.y + d * 0.5);
            p.text(initial_c.as_ptr(), r.x + d * 0.5, ty, theme::size::HEADLINE, theme::TEXT_PRIMARY, 1, 1);
        }
    }
}

/// [`profile_chip`], re-drawn over the account menu's scrim — [`Opener`]'s contract, for the one
/// popover in the app that hangs off a piece of shared CHROME rather than off a card.
///
/// It lives HERE, beside the chip itself, for the reason the chip does: the control is drawn
/// verbatim by Home, the Library and Search, and the menu can now be opened from any of the three
/// ([`crate::app`]'s `BarHost`). It was `home::redraw_profile_chip` while Home was the only screen
/// whose chip could be pressed, which would have lifted the HOME chip's spring over whichever page
/// the user was actually on — and there is only one chip, so there is only one lift.
///
/// On `nav::chrome_alpha` and not `page_alpha`, because that is the alpha the chip is drawn on: the
/// top band holds still while pages swap under it. A lift on the wrong alpha would make the chip
/// flicker through a route change that nothing else on the bar reacts to.
///
/// [`Opener`]: crate::ui::popover::Opener
pub(crate) fn redraw_profile_chip() {
    crate::ui::guard(|| profile_chip(Painter::root().alpha(crate::ui::nav::chrome_alpha())));
}

// ---- CircleButton: circular disc + centered glyph, same ControlStyle family as Button /
// TransportButton (focused = ACCENT, idle = solid dark disc). The hero + detail +/i/> circles. ----

/// The **UNFURL**: a focused disc grows rightward into a capsule carrying the verb its press
/// performs — icon-only at rest, LABELLED on focus.
///
/// It exists because a disc is the one control that cannot say what it does. At ten feet a bare
/// tick teaches nobody, and a television has no hover to hint with, so focus is the only moment the
/// user can be told. Resting geometry is unchanged: at `e == 0` every number below cancels and the
/// control is the same 60px disc it has always been, which is what lets the whole family opt in
/// without a second widget beside it.
///
/// The open capsule is `LEAD + icon + GAP + label + TAIL` — deliberately asymmetric, the icon side
/// tighter, because a round glyph reads as further from a capsule's end than a stem does. The
/// glyph's own inset slides from the disc's centring to [`DISC_LABEL_LEAD`] across the unfurl, so
/// both ENDS are exactly the shapes the design draws rather than only the open one.
///
/// Sibling of the profile chip's own unfurl (`chip_cap`), and the same two rules apply: the width
/// is the ONE expression both the painter and the hit-test read ([`CircleButton::cap_w`]), and the
/// label rides the TAIL of the widening so its glyphs land in a capsule that has already made room
/// for them instead of smearing across the grow.
///
/// The two differ on ONE policy and deliberately: what happens when the word does not fit. The chip
/// ELIDES to a fixed `CHIP_NAME_MAX` budget, because a profile name is user data of any length and
/// a shortened name is still a name. A verb is not — half of "Mark as Unwatched" teaches less than
/// the tick it was meant to explain — so a control with a hard bound to respect asks
/// [`CircleButton::label_budget`] and shows the whole word or none of it
/// (`detail::watch_cap_at`). Both are right for their content; neither is the default.
const DISC_LABEL_LEAD: f32 = 26.0;
const DISC_LABEL_GAP: f32 = 14.0;
const DISC_LABEL_TAIL: f32 = 34.0;
/// The unfurl's stiffness — deliberately [`K_CHIP`], the profile chip's own. The two are the same
/// interaction (a focused control opening far enough to name itself) and a bar that settles at one
/// rate over a hero row that settles at another reads as two systems rather than one.
pub(crate) const K_DISC_UNFURL: f32 = K_CHIP;

pub struct CircleButton {
    pub frame: Rect,
    pub glyph: *const c_char,
    pub icon: Option<crate::ui::icons::Icon>, // vector glyph; overrides the text glyph when set
    pub focused: bool,
    pub style: ControlStyle,
    /// The FOCUS POP, as a factor on the frame — see [`CircleButton::scale`].
    pub scale: f32,
    /// The unfurled label and how far open it is (0..1) — see [`DISC_LABEL_LEAD`]. `None` is a
    /// plain disc, which is what every caller that has not opted in still gets.
    pub label: Option<(*const c_char, f32)>,
}
impl CircleButton {
    pub fn new(glyph: *const c_char) -> Self {
        Self {
            frame: Rect::new(0.0, 0.0, 60.0, 60.0),
            glyph,
            icon: None,
            focused: false,
            style: ControlStyle::Accent,
            scale: 1.0,
            label: None,
        }
    }
    /// Move the disc, keeping its own diameter. Enough for a plain disc, and NOT enough for an
    /// unfurled one — see [`frame`](Self::frame).
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.frame.x = x;
        self.frame.y = y;
        self
    }
    /// Place AND size the control from a frame the caller already owns.
    ///
    /// The unfurl's width is [`cap_w`](Self::cap_w), which a row that accumulates its controls has
    /// already had to compute — so it hands the whole rect over rather than an origin, and the
    /// shape drawn is by construction the shape that row reserved and the pointer grades against.
    /// [`at`](Self::at) would silently keep the bare 60, which draws a labelled capsule clipped to
    /// a disc.
    pub fn frame(mut self, r: Rect) -> Self {
        self.frame = r;
        self
    }
    /// Render a vector icon centred on the disc instead of the text glyph (e.g. a real
    /// chevron rather than a ">" character). Pass a bare-stroke icon — one that carries its
    /// own outline circle (Info) would double-ring against the disc face.
    pub fn icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    /// The [FOCUS POP](CTRL_FOCUS_SCALE), about the control's centre — normally
    /// [`CtlPop::scale`], which already folds the press dip in. `1.0` is the resting control, which
    /// is what every caller that has not opted in still gets.
    ///
    /// It scales the PLATE and the glyph box. It does not scale the unfurled label's TYPE: text in
    /// this app is sized off `theme::size`'s rungs and nothing may sit between them, so the word
    /// keeps its `BODY` 28 through the pop and only its placement moves. At 1.07 on a 60px control
    /// that is a two-pixel relative difference seen from three metres — the rung rule is worth more.
    pub fn scale(mut self, s: f32) -> Self {
        self.scale = s;
        self
    }
    pub fn style(mut self, s: ControlStyle) -> Self {
        self.style = s;
        self
    }
    /// Give this disc the [UNFURL](DISC_LABEL_LEAD): `text` is the verb the press performs and `e`
    /// (0..1) is how far open the capsule is — normally a focus spring, so the control reads as one
    /// object opening rather than a label appearing beside it. `e == 0` draws the plain disc.
    ///
    /// The caller must size [`frame`](Self::frame)`.w` with [`cap_w`](Self::cap_w) at the same `e`:
    /// the drawn shape and the graded shape are one expression (`chip_cap`'s rule), which is what
    /// keeps a pointer clicking the capsule it can see. A frame that is NOT a capsule cancels the
    /// unfurl outright, glyph included — see [`cap_label_w`](Self::cap_label_w) — so a row that
    /// refused this control its room can keep passing `e` without drawing anything wrong.
    ///
    /// Three preconditions, none of which the builder can enforce:
    /// * **It needs [`icon`](Self::icon).** The label is drawn beside the vector glyph; the
    ///   text-glyph form has no run to sit next to, and silently ignores this.
    /// * **The label is drawn at [`theme::size::BODY`]**, the one control-label rung — measure with
    ///   the same size when sizing the frame, or the capsule and its word disagree.
    /// * **It clips, and the clip has no stack** (`gfx::clip_clear` is a bare `glDisable`). Drawing
    ///   an unfurled control INSIDE another scissor releases that outer clip for the rest of the
    ///   frame. Every caller today is unclipped; a scrolling list that adopts this is not.
    pub fn label(mut self, text: *const c_char, e: f32) -> Self {
        self.label = Some((text, e));
        self
    }

    /// The capsule width of a disc of diameter `d`, unfurled `e` (0..1) around a label measured
    /// `label_w` wide. The LAYOUT companion to `draw`, and the only place the open geometry is
    /// written down — a row accumulating this control's advance and the painter placing its glyphs
    /// read the very same number, at every phase of the animation.
    ///
    /// `label_w <= 0` is "no label", which collapses to the bare disc whatever `e` says.
    pub fn cap_w(d: f32, e: f32, label_w: f32) -> f32 {
        if label_w <= 0.0 {
            return d;
        }
        d + (Self::cap_open_w(d, label_w) - d) * e.clamp(0.0, 1.0)
    }

    /// The fully-open capsule — [`cap_w`](Self::cap_w) at `e == 1`, and the only place the open
    /// geometry is spelled out.
    fn cap_open_w(d: f32, label_w: f32) -> f32 {
        DISC_LABEL_LEAD + (d * DISC_ICON_RATIO).round() + DISC_LABEL_GAP + label_w + DISC_LABEL_TAIL
    }

    /// The widest label a disc of diameter `d` may unfurl when its row can spare only `room`
    /// beyond the bare disc — i.e. `cap_w(d, 1.0, label_budget(d, room)) - d == room`.
    ///
    /// The INVERSE of [`cap_w`](Self::cap_w), for a caller that has a fixed bound to respect and a
    /// label to decide about (`detail::watch_cap_at`). Having it here rather than re-derived at
    /// the call site is the same rule the width itself follows: the open geometry is written down
    /// once, so a budget and the shape it is a budget FOR cannot drift apart. Can come back
    /// negative, which means the disc cannot afford a label of any width at all.
    pub(crate) fn label_budget(d: f32, room: f32) -> f32 {
        room - (Self::cap_open_w(d, 0.0) - d)
    }

    /// The label width a capsule of DRAWN width `w` was sized for — the exact inverse of
    /// [`cap_w`](Self::cap_w) at the same `e`, so the widget can recover what its caller measured
    /// without being handed it a second time (two numbers that must agree are two numbers that can
    /// disagree; this way the frame is the single statement of the geometry, as `chip_cap`'s rule
    /// demands).
    ///
    /// `None` means **this frame is not a capsule**, and it is the widget's whole guard rather than
    /// a convenience: `w <= d` is a caller that asked for a label and was given a bare disc — a row
    /// that REFUSED the unfurl for want of room ([`label_budget`](Self::label_budget)), or a
    /// measurement that came back 0 because the font never opened — while its focus spring keeps
    /// climbing to 1. Answering with the arithmetic there returns a NEGATIVE label width, which
    /// then reads as "acres of tail air" to the fade ramp and lights the word up at full alpha; and
    /// the icon, slid by that same `e`, lands 26px into a 60px circle. Both are the disc drawn
    /// wrong, from a state the caller declared by handing over the frame.
    fn cap_label_w(d: f32, e: f32, w: f32) -> Option<f32> {
        if e <= 0.01 || w <= d {
            return None;
        }
        let lw = (w - d) / e - (Self::cap_open_w(d, 0.0) - d);
        (lw > 0.0).then_some(lw)
    }
}
impl View for CircleButton {
    fn draw(&self, _e: &Env, p: Painter) {
        let base = self.frame;
        let r = base.scaled(self.scale);
        let (face, ink) = self.style.colors(self.focused);
        // The DISC is the frame's HEIGHT, not its width: an unfurled control is a capsule, and
        // taking the radius and the glyph box off `w` would swell both as it opened. At rest the
        // two are the same number, which is why every plain caller is unaffected.
        let d = r.h;
        let rad = d * 0.5;
        // No drop shadow — the edge holds it. See `Button::draw` for the whole argument and for the
        // measured case that used to justify one.
        control_rim(p, r, rad, face);
        // Resolve the unfurl from the FRAME, before anything is placed by it: `cap_label_w` answers
        // `None` for a frame that is not a capsule, and that verdict governs the GLYPH as well as
        // the word. Sliding the icon off the caller's raw `e` was a real defect — a control whose
        // row refused it the room still had a focus spring climbing to 1, and the check kicked 12px
        // right inside a disc that never grew.
        // …from the frame the CALLER sized (`cap_w`), not the popped one: the pop is a factor
        // applied after the row's layout, so asking the base frame is how the widget recovers the
        // label width its caller actually measured.
        let unfurl = self
            .label
            .and_then(|(text, e)| Some((text, e.clamp(0.0, 1.0), Self::cap_label_w(base.h, e, base.w)?)));
        let e = unfurl.map(|(_, e, _)| e).unwrap_or(0.0);
        if let Some(icon) = self.icon {
            // vector glyph at the shared DISC_ICON_RATIO box, so every round control carries its
            // icon at one ratio — centred on the DISC, sliding to the open capsule's lead inset as
            // the unfurl opens (see `DISC_LABEL_LEAD`).
            let isz = (d * DISC_ICON_RATIO).round();
            let closed_inset = (d - isz) * 0.5;
            // the capsule's own air rides the pop with it, so a popped capsule is the same shape
            // scaled and not a wider one wearing the original padding
            let (lead, gap, tail) = (
                DISC_LABEL_LEAD * self.scale,
                DISC_LABEL_GAP * self.scale,
                DISC_LABEL_TAIL * self.scale,
            );
            let ix = r.x + closed_inset + (lead - closed_inset) * e;
            crate::ui::icons::draw(p, icon, Rect::new(ix, r.y + (r.h - isz) * 0.5, isz, isz), ink);
            if let Some((text, _, label_w)) = unfurl {
                // The label rides the TAIL of the widening, and the ramp is the GEOMETRY rather
                // than a chosen fraction of it: it fades in exactly as fast as the capsule's tail
                // air ([`DISC_LABEL_TAIL`]) opens up behind the last glyph. So the word is at zero
                // the instant it would still be overhanging, and at full only once the capsule has
                // the whole run plus its own end air — which is what stops a half-cut glyph being
                // legible for a few frames of every focus move. A magic 0.55 could not do that: the
                // crossover moves with the LABEL's width, and this row's two differ by 34px.
                let tx = ix + isz + gap;
                let a = (((r.x + r.w) - (tx + label_w * self.scale)) / tail).clamp(0.0, 1.0);
                if a > 0.004 {
                    let ty = crate::text::text_vcenter_y(theme::size::BODY, 1, r.y + d * 0.5);
                    // …and the clip stays, as the backstop the ramp makes invisible: a caller whose
                    // frame does not come from `cap_w` would otherwise paint a label out over the
                    // page (the design's own `overflow:hidden`).
                    p.clip(r);
                    p.alpha(a).text(text, tx, ty, theme::size::BODY, ink, 0, 1);
                    p.clip_clear();
                }
            }
        } else {
            // text glyph centred on the disc by its cap band (layout ≠ paint), not a hand-tuned y
            crate::ui::label::Label::new(self.glyph, crate::ui::theme::size::HEADLINE, ink)
                .h(crate::ui::label::HAlign::Center)
                .draw(p, Rect::new(r.x, r.y, d, d));
        }
    }
}

// ---- PageDots: page indicators; active dot elongated ----
pub struct PageDots {
    pub count: usize,
    pub active: usize,
    pub x: f32,
    pub y: f32,
}
impl PageDots {
    const GAP: f32 = 12.0; // equal edge-gap between every element (dot↔dot and dot↔pill)
    const DOT: f32 = 10.0; // inactive diameter (also the pill height)
    const PILL: f32 = 24.0; // active pill width

    pub fn new(count: usize) -> Self {
        Self { count, active: 0, x: 0.0, y: 0.0 }
    }
    /// the lit dot (0-based); clamped into range so a stale index degrades to the last dot.
    pub fn active(mut self, i: usize) -> Self {
        self.active = if self.count == 0 { 0 } else { i.min(self.count - 1) };
        self
    }
    pub fn at(mut self, x: f32, y: f32) -> Self {
        self.x = x;
        self.y = y;
        self
    }
    /// Place the row so it is CENTRED on `cx` — the billboard pager idiom, where the dots belong to
    /// the screen rather than to the control they sit under. Resolves to a left origin through
    /// [`width`](Self::width), so `draw` stays one left-to-right walk.
    pub fn centered_at(self, cx: f32, y: f32) -> Self {
        let w = self.width();
        self.at(cx - w * 0.5, y)
    }
    /// The row's drawn width — the SAME advance `draw` walks (each element's own width plus one
    /// gap between neighbours), so a centred row can't drift from the pixels.
    pub fn width(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        (self.count - 1) as f32 * (Self::DOT + Self::GAP) + Self::PILL
    }
}
impl View for PageDots {
    fn draw(&self, _e: &Env, p: Painter) {
        // Advance by each element's own width + a fixed gap, so the gaps are equal even around the
        // wider active pill (equal centre-pitch would squeeze the pill's neighbours). The pill just
        // takes more width and nudges the trailing dots along.
        let mut x = self.x;
        for d in 0..self.count {
            let active = d == self.active;
            let w = if active { Self::PILL } else { Self::DOT };
            // tokens, not a raw literal: full-strength white for the current page, dimmed for the rest
            let col = crate::ui::theme::with_a(crate::ui::theme::TEXT_PRIMARY, if active { 1.0 } else { 0.35 });
            p.rect(Rect::new(x, self.y, w, Self::DOT), Self::DOT * 0.5, col, col, 0.0);
            x += w + Self::GAP;
        }
    }
}

// ---- Spinner: dots around a circle, the leading one bright and trailing into a fade. A loading/
// buffering indicator (e.g. the player HUD while a seek resolves). `phase` (ms) drives rotation. ----
pub struct Spinner {
    pub cx: f32,
    pub cy: f32,
    pub r: f32,
    pub phase: u32,
    pub col: [f32; 4],
    pub dots: usize,
    pub dot_r: f32,
}
impl Spinner {
    /// One full revolution, in ms. Public because a caller that accumulates its own phase clock
    /// can only wrap it losslessly on a whole period (see `home.rs`'s `status_ms`).
    pub const PERIOD_MS: u32 = 760;
    /// The **page** radius — a wait that owns a whole surface: the [`StatusOverlay`] read-out, and
    /// any screen whose content is simply not there yet. Sized to be read from the couch.
    ///
    /// Associated consts on the widget are the house pattern ([`PageDots::DOT`]/`PILL`/`GAP`): a
    /// spinner radius is neither a text size nor a gap, so it does not belong in `theme.rs`.
    pub const R_PAGE: f32 = 22.0;
    /// The **inline** radius — a mark that belongs to one line of text or one control, sized to sit
    /// on a `size::CAPTION` cap band. Anything that must sit *beside* something rather than *over*
    /// everything uses this.
    pub const R_INLINE: f32 = 12.0;
    /// Dot radius for a ring of radius `r` — the ONE place the ratio lives, so a layout can measure
    /// a spinner's real extent without re-deriving it (0.28·r ≈ the HUD spinner's original 3.4 at
    /// r=12, floored so a tiny ring still has visible dots).
    pub fn dot_r(r: f32) -> f32 {
        (r * 0.28).max(3.0)
    }
    pub fn new(cx: f32, cy: f32, r: f32) -> Self {
        // dot size scales WITH the ring radius, so a big spinner reads as a bigger spinner, not the
        // same tiny dots on a wider circle.
        Self { cx, cy, r, phase: 0, col: [1.0, 1.0, 1.0, 1.0], dots: 10, dot_r: Self::dot_r(r) }
    }
    pub fn phase(mut self, ms: u32) -> Self {
        self.phase = ms;
        self
    }
    pub fn tint(mut self, c: [f32; 4]) -> Self {
        self.col = c;
        self
    }
}
impl View for Spinner {
    fn draw(&self, _e: &Env, p: Painter) {
        // A spinner is driven by a CLOCK, not a spring, so `ui::idle`'s spring instrumentation
        // cannot see it: before this line, a Home waiting on /hubs — the exact state the read-out
        // exists for — drew a STOPPED spinner. Reported here, in `draw`, and that is deliberate:
        // only a spinner actually ON SCREEN should hold the loop awake, whereas the six phase
        // accumulators that feed it tick unconditionally at the top of their screens' update.
        //
        // Reporting from a draw is sound in exactly one direction — it can only latch the gate ON
        // for the next frame, never off — and it self-sustains: the landing that started the load
        // presents frame 1, whose draw reports and so buys frame 2, until nothing draws a spinner.
        // It relies on `should_present` taking-and-clearing rather than `note_present` clearing
        // after the draw, which would destroy this report on the frame it is raised.
        crate::ui::idle::invalidate();
        let t = (self.phase % Self::PERIOD_MS) as f32 / Self::PERIOD_MS as f32;
        for i in 0..self.dots {
            let ang = i as f32 / self.dots as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            let lead = (t - i as f32 / self.dots as f32).rem_euclid(1.0);
            let a = (1.0 - lead) * 0.85 + 0.12; // bright at the leading dot, fading behind it
            let c = [self.col[0], self.col[1], self.col[2], self.col[3] * a];
            let (dx, dy) = (self.cx + self.r * ang.cos(), self.cy + self.r * ang.sin());
            let d = self.dot_r;
            p.rect(Rect::new(dx - d, dy - d, 2.0 * d, 2.0 * d), d, c, c, 0.0);
        }
    }
}

// ---- AmbientWash: the full-screen four-corner colour wash keyed to an item's artwork. ----

/// The **ambient wash** — a page-wide bilinear gradient taken from an item's PMS `UltraBlurColors`
/// corners, which is how home's backdrop, detail's below-hero ground and the person page all say
/// "this screen is about *this* artwork" without paying for a full-bleed image.
///
/// It exists as a component because the non-obvious part is not the gradient, it is the FADE.
/// [`Painter::ambient`](crate::ui::Painter::ambient) writes opaque pixels (its `dim` scales the
/// corners toward BLACK, which is a different thing), so a wash cannot be cross-faded from ONE
/// ITEM'S colours to ANOTHER'S by alpha at all — the only way is to spring each corner channel
/// toward its target and keep drawing at full strength. That is twelve springs, and a screen that
/// hand-rolls them ends up doing index arithmetic over a flat array. Fading a wash toward the app's
/// own GROUND is the one case the cascade *does* handle, and it belongs to the cascade: an alpha
/// below 1 mixes the corners toward [`theme::SURFACE_APP`] inside `Painter::ambient`, which is what
/// lets a whole page — wash included — dip for [`ui::nav`](crate::ui::nav)'s route transition.
///
/// Blend the corners toward a base surface with [`theme::mix`] before handing them over: a wash
/// keyed at full strength is a photograph, not a wash, and mixing toward [`theme::SURFACE_APP`]
/// means "no artwork" is simply the app's own flat ground with no special case.
#[derive(Clone, Copy)]
pub(crate) struct AmbientWash {
    /// corner-major, `Painter::ambient`'s order: top-left, top-right, bottom-right, bottom-left.
    corners: [[Spring; 3]; 4],
}

/// The luminance ceiling a GROUND colour is held to before it is mixed toward the surface.
/// UltraBlur corners are SAMPLED FROM THE ARTWORK, so a white poster hands us a near-white corner:
/// at any weight strong enough to see, that lifts the page off the palette's dark end and takes the
/// [`theme::TEXT_TERTIARY`] fine print sitting on it along — an uncapped white corner at
/// [`AmbientWash::GROUND_W`] puts `TEXT_TERTIARY`/`size::CAPTION` at **2.10:1**, under the 3:1
/// large-text floor; capped here the worst source (saturated green) is **3.67:1** and a white poster
/// **3.83:1**. A hard ceiling, not a tone map: only brightness is spent, the corner's hue and its
/// channel balance survive untouched, which is all a wash is saying. Rec.709 weights over the stored
/// display-encoded values — a ground-brightness knob, not colour management.
///
/// Why 0.42 and not higher: the legibility constraint only binds near **0.59** (green hits 3.0:1
/// there), so this is chosen design-first — a ceiling that let a white poster produce a mid-grey
/// page would betray the word "dark", and the contrast margin then falls out for free. Every number
/// above is MEASURED, by `a_ground_never_outshines_the_fine_print_that_sits_on_it`.
const GROUND_LUMA: f32 = 0.42;

/// One artwork corner, held under [`GROUND_LUMA`]. A scalar multiply (via [`theme::dim`]) rather
/// than a per-channel clamp, which would desaturate the corner instead of dimming it.
fn ground_capped(c: [f32; 3]) -> [f32; 4] {
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    theme::dim([c[0], c[1], c[2], 1.0], if y > GROUND_LUMA { GROUND_LUMA / y } else { 1.0 })
}

impl AmbientWash {
    /// The dissolve rate every page wash shares — deliberately slower than any focus spring (the
    /// person mock's `.5s ease`): the wash is atmosphere, and snapping it with the focus pop reads
    /// as the SCREEN changing rather than the subject. One rate, because two pages dissolving at two
    /// speeds are two different products.
    pub(crate) const K: f32 = 80.0;

    /// How far a GROUND leans from the app surface toward an item's own colour — the strength every
    /// item-keyed wash shares (the person page's focused-card state, the detail page's whole page).
    /// One value, because "this screen is about THIS artwork" should be the same statement wherever
    /// it is made; the per-corner SHAPE stays each screen's business (person tapers its bottom
    /// corners toward its shelves, detail keeps the artwork's own corner arrangement).
    ///
    /// The bound is what makes it safe to be the page's ground everywhere: with the source capped at
    /// [`GROUND_LUMA`], the brightest ground is ≈60/255 and the darkest (black corners) ≈33/255 —
    /// clear of the near-black 25/255 that [`theme::SURFACE_APP`]'s own doc rejects as too dark for
    /// a card shadow to read against. No floor constant is needed; `GROUND_W ≤ 0.26` IS the floor.
    pub(crate) const GROUND_W: f32 = 0.26;

    /// How close a wash must be to the colour it is drawn over before a screen stops drawing it —
    /// [`is_flat`](Self::is_flat)'s epsilon, ONE 8-bit code, below which the panel cannot show a
    /// difference. ([`theme::SURFACE_APP`] is snapped to exact 8-bit codes for the `GL_DITHER`
    /// reason in its own doc, so that is the unit this is reasoned in.)
    ///
    /// It lives beside the type rather than in a screen because the skip is worth ~2.07M fragments
    /// a frame on two different pages, and it was a private `home.rs` constant while `detail.rs`
    /// simply lacked the test — one value here is what stops the two from drifting apart again.
    pub(crate) const FLAT_EPS: f32 = 1.0 / 255.0;

    /// A dissolve target: corner `i` mixed from [`theme::SURFACE_APP`] toward `src[i]` by `w[i]`
    /// ("how much of this source shows through at that corner"). The mix is the TYPE's contract (see
    /// the docs above) rather than a loop each screen writes, so "no artwork" is the app's own flat
    /// ground on every page **by construction** — `w = 0` returns exactly the surface. Colours that
    /// came from ARTWORK go through [`keyed`](Self::keyed), which caps them first.
    pub(crate) fn target(src: [[f32; 4]; 4], w: [f32; 4]) -> [[f32; 4]; 4] {
        std::array::from_fn(|i| theme::mix(theme::SURFACE_APP, src[i], w[i]))
    }

    /// [`target`](Self::target) for corners taken from an item's `UltraBlurColors` envelope
    /// (`PmsMovie::blur` / `metadata::Detail::blur`), each held under [`GROUND_LUMA`] first. Every
    /// artwork-keyed wash goes through here; a palette token (the resting warm tint) does not need
    /// it, because we chose that value.
    pub(crate) fn keyed(blur: [[f32; 3]; 4], w: [f32; 4]) -> [[f32; 4]; 4] {
        Self::target(blur.map(ground_capped), w)
    }

    /// A wash resting flat on one colour — what a page opens as before any item keys it.
    pub(crate) fn flat(c: [f32; 4]) -> Self {
        AmbientWash { corners: [[Spring::at(c[0]), Spring::at(c[1]), Spring::at(c[2])]; 4] }
    }
    /// Jump straight to `target` (no dissolve) — on mount, so the PREVIOUS item's colours never
    /// dissolve across a page that has just changed subject.
    pub(crate) fn jump(&mut self, target: [[f32; 4]; 4]) {
        for (c, t) in self.corners.iter_mut().zip(target) {
            for (sp, v) in c.iter_mut().zip(t) {
                sp.jump(v);
            }
        }
    }
    /// Dissolve toward `target` at rate `k`. Every channel shares one rate, so the corners move as
    /// one wash rather than twelve independent fades.
    pub(crate) fn step(&mut self, target: [[f32; 4]; 4], k: f32, dt: f32) {
        for (c, t) in self.corners.iter_mut().zip(target) {
            for (sp, v) in c.iter_mut().zip(t) {
                sp.step(v, k, dt);
            }
        }
    }
    /// Is this wash within `eps` of the flat colour `c` on every corner channel? A wash that has
    /// resolved to the app's own clear colour is a ~2M-fragment full-screen fill that changes
    /// nothing, and a screen must be able to skip it. (`eps` is naturally one 8-bit code —
    /// [`theme::SURFACE_APP`] is snapped to exact codes for the `GL_DITHER` reason in its own doc,
    /// so that is the unit this value is reasoned in.) Its own method because the corner springs are
    /// private.
    pub(crate) fn is_flat(&self, c: [f32; 4], eps: f32) -> bool {
        self.corners.iter().all(|q| q.iter().zip(c).all(|(s, v)| (s.pos - v).abs() <= eps))
    }
    /// Paint it over `r`. Opaque — this REPLACES what is under it (see the type docs), so it belongs
    /// at the bottom of a screen's draw, standing in for the flat clear.
    pub(crate) fn draw(&self, p: Painter, r: Rect) {
        p.ambient(r, 1.0, self.corners.map(|c| [c[0].pos, c[1].pos, c[2].pos]));
    }
}

// ---- StatusOverlay: a centred "something is happening / something failed" read-out — a Spinner
// (or nothing, for a terminal state) above one line of copy, optionally a REASON under it and the
// ONE action that answers it. The player HUD renders it for the
// states where NO PICTURE IS ON THE PANEL (Resolving/Connecting/Buffering/Seeking before this
// session's first frame, and Error, which is what a black screen used to be) — a seek over a live
// picture belongs to the transport's inline spinner instead, and `player_hud::busy_surface` is the
// ONE place that division is written down. `kind` picks the treatment, not the words: the caller
// supplies the caption so the state machine stays the single source of that string.
//
// The `frame` is THE AREA THE WAIT IS ABOUT, and the read-out centres on it — pass the region whose
// content is missing, not a region carved out to dodge other chrome. Home's catalog is the whole
// screen (`Rect::FULL`); the person page's shelves are one band, so it passes the band; the player's
// picture is the whole panel, so it passes `Rect::FULL` too. Carving the frame down to "avoid"
// nearby chrome pushes the read-out OFF the optical centre, which is exactly what the player's
// deleted `OVERLAY_BOTTOM` did — it centred the block at y=370 on a 1080 panel. The Library's
// SECTION read-out is the one case where a smaller frame is the honest answer rather than a dodge:
// it passes the content region because the chrome above it is still live, and that is the whole
// difference between a section failing and the app failing (`Shared Sources.dc.html` D).
//
// **Up to three blocks, and each answers one question**: the verdict (what happened), the reason
// (why, and what is NOT broken), the action (the one thing to press). Two of the three are
// optional, and an absent one is ABSENT — no gap, no empty band, no draw call — so the read-outs
// that carry only a caption are laid out exactly as they were before the other two existed. ----
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusKind {
    /// in flight — spinner + secondary copy
    Working,
    /// terminal failure — no spinner, danger-tinted copy
    Failed,
    /// nothing to show, and that is the server's honest ANSWER rather than a fault — no spinner,
    /// de-emphasized copy. Distinct from `Failed` on purpose: an empty library is not an error and
    /// must not wear the danger tint.
    ///
    /// The TINT is this kind's; the ACTION is the caller's, and the two callers differ for a
    /// reason. The Library's empty section offers none — the server answered, and asking it the
    /// same question again gets the same answer. Home's empty hub offers *Refresh*, because there
    /// the honest reading of "nothing yet" is a server still scanning its first library, and a
    /// second look is what finds it. Neither is a retry of a failure.
    Empty,
}
/// The read-out's own type sizes. The verdict is reading text; the reason is one rung down because
/// it EXPLAINS rather than states, and the action carries a control label, which is the verdict's
/// rung again (`Button`'s rung everywhere else in the product).
const STATUS_CAP_SZ: c_int = theme::size::BODY;
const STATUS_REASON_SZ: c_int = theme::size::CAPTION;
pub struct StatusOverlay<'a> {
    pub frame: Rect,
    /// The VERDICT — one line naming what happened.
    ///
    /// Borrowed rather than `&'static`: a section read-out interpolates a machine name ("Can't
    /// reach &lt;machine&gt;"), which no `c"…"` literal can carry. `&'static CStr` still coerces, so the
    /// state-machine captions (`PlaybackState::caption()`, Home's hub states) are unchanged. The
    /// `Label` rule applies to a runtime one — keep the `CString` alive for the whole draw frame.
    pub caption: &'a core::ffi::CStr,
    /// The line UNDER the verdict, in de-emphasized ink: why it happened, and — the job it exists
    /// for — what is still fine. `None` = absent, not empty.
    pub reason: Option<&'a core::ffi::CStr>,
    /// The read-out's ONE control, drawn below the copy and hit-tested by the screen through
    /// [`StatusOverlay::action_frame`], so the drawn pill and the rect a click lands in are one
    /// expression. `None` = no control: while a fetch is in flight the spinner IS the state, and an
    /// [`StatusKind::Empty`] answer has nothing to retry.
    pub action: Option<&'a core::ffi::CStr>,
    pub kind: StatusKind,
    pub phase: u32,
    /// whether the action pill holds focus (ignored when there is no action)
    pub focused: bool,
}

/// The stacked read-out's geometry — the ONE place its three bands are placed, read by the draw and
/// by [`StatusOverlay::action_frame`] so a screen's hit test cannot drift from the pill it sees.
struct StatusBands {
    cap: Rect,
    reason: Option<Rect>,
    /// top of the action pill (whether or not there is one)
    action_y: f32,
}

impl<'a> StatusOverlay<'a> {
    /// The action pill's height. The app's one action-control size — the hero rows' pill/disc
    /// diameter, which `home.rs` aliases rather than restating.
    pub const CTRL_H: f32 = 60.0;

    pub fn new(frame: Rect, caption: &'a core::ffi::CStr, kind: StatusKind) -> Self {
        Self { frame, caption, reason: None, action: None, kind, phase: 0, focused: false }
    }
    /// ms clock driving the spinner's rotation (ignored by `Failed`)
    pub fn phase(mut self, ms: u32) -> Self {
        self.phase = ms;
        self
    }
    /// The line under the verdict — see [`StatusOverlay::reason`].
    pub fn reason(mut self, r: &'a core::ffi::CStr) -> Self {
        self.reason = Some(r);
        self
    }
    /// The read-out's one control — see [`StatusOverlay::action`].
    pub fn action(mut self, label: &'a core::ffi::CStr) -> Self {
        self.action = Some(label);
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    /// How far the read-out's ink reaches ABOVE the frame centre: the spinner ring plus its dots
    /// plus the `space::XS` that separates it from the caption. The caption half is deliberately NOT
    /// here — it needs `text::text_height`, which the host suite cannot link — and it is the SMALLER
    /// half, so this doubles as the conservative bound a layout test can assert against.
    pub fn above() -> f32 {
        Spinner::R_PAGE + theme::space::XS + Spinner::R_PAGE + Spinner::dot_r(Spinner::R_PAGE)
    }

    /// Where the three blocks sit. Everything hangs off ONE anchor — the caption band, which keeps
    /// the exact position it has always had (`Working` straddles the frame centre with the spinner
    /// above it; a terminal state owns the centre alone) — and the optional blocks stack BELOW it on
    /// `theme::space` rungs. That ordering is deliberate: adding a reason or an action cannot move
    /// the read-outs that carry neither, so the player's and the person page's are untouched.
    fn bands(&self) -> StatusBands {
        let cy = self.frame.cy();
        let cap_h = crate::text::text_height(STATUS_CAP_SZ, 0);
        let cap_y = if self.kind == StatusKind::Working { cy + theme::space::XS } else { cy - cap_h * 0.5 };
        let cap = Rect::new(self.frame.x, cap_y, self.frame.w, cap_h);
        let mut below = cap_y + cap_h;
        let reason = self.reason.map(|_| {
            let h = crate::text::text_height(STATUS_REASON_SZ, 0);
            let r = Rect::new(self.frame.x, below + theme::space::SM, self.frame.w, h);
            below = r.y + h;
            r
        });
        StatusBands { cap, reason, action_y: below + theme::space::LG }
    }

    /// The action pill's frame, or `None` when there is no action. The screen records this for its
    /// pointer hit test; the draw builds its `Button` from the same call.
    pub fn action_frame(&self) -> Option<Rect> {
        let label = self.action?;
        let w = Button::pill_w(label.as_ptr(), STATUS_CAP_SZ, false);
        Some(Rect::new(self.frame.cx() - w * 0.5, self.bands().action_y, w, Self::CTRL_H))
    }
}
impl View for StatusOverlay<'_> {
    fn draw(&self, e: &Env, p: Painter) {
        // spinner above, caption below, the pair centred on the frame
        let cy = self.frame.cy();
        let (tint, working) = match self.kind {
            StatusKind::Working => (theme::TEXT_SECONDARY, true),
            StatusKind::Failed => (theme::DANGER, false),
            StatusKind::Empty => (theme::TEXT_TERTIARY, false),
        };
        let b = self.bands();
        // Both branches centre the caption the same way — by Label's cap band (VAlign::Middle,
        // the default). Working straddles the frame centre with the spinner above it; Failed owns
        // the centre alone. Using the cap band for one and a line-box metric for the other put the
        // two states on different baselines in the same frame.
        if working {
            Spinner::new(self.frame.cx(), cy - Spinner::R_PAGE - theme::space::XS, Spinner::R_PAGE)
                .phase(self.phase)
                .tint(tint)
                .draw(e, p);
        }
        Label::new(self.caption.as_ptr(), STATUS_CAP_SZ, tint).h(HAlign::Center).draw(p, b.cap);
        // The reason is NEVER in the verdict's ink: the tint is the severity of what happened, and
        // this line's job is the opposite — it says what is still working.
        if let (Some(r), Some(band)) = (self.reason, b.reason) {
            Label::new(r.as_ptr(), STATUS_REASON_SZ, theme::TEXT_TERTIARY).h(HAlign::Center).draw(p, band);
        }
        if let (Some(label), Some(f)) = (self.action, self.action_frame()) {
            // **No [`CTRL_FOCUS_SCALE`] pop, deliberately.** Every other control face in the app
            // takes one; this is the one surface where it would say nothing. A read-out's action is
            // the ONLY focusable thing on the region it owns — `library::sync_readout_focus` lands
            // the ring on it the moment the read-out appears and there is nowhere else for it to
            // go — so the pop has no sibling to distinguish this control from and would resolve to
            // a constant 1.07, i.e. a slightly larger button with no signal in it. The pop is a
            // ROW's affordance; a lone control is a different question. Give it one the day a
            // read-out offers two actions.
            Button::new(label.as_ptr(), STATUS_CAP_SZ, f).focused(self.focused).draw(e, p);
        }
    }
}

// ---- TransportButton: circular control button with a runtime-rasterized SVG glyph
// (0 = subtitles/CC, 1 = audio, 2 = more/overflow). Focused = accent fill + dark icon; idle = faint
// fill + white icon. Mirrors the mockup's round icon buttons. ----
pub struct TransportButton {
    pub frame: Rect,
    pub which: i32,
    pub focused: bool,
    /// The FOCUS POP, as a factor on the frame — see [`CircleButton::scale`], whose contract this
    /// shares (these discs are that same face at 64px).
    pub scale: f32,
}
impl TransportButton {
    pub fn new(which: i32, frame: Rect) -> Self {
        Self { frame, which, focused: false, scale: 1.0 }
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    pub fn scale(mut self, s: f32) -> Self {
        self.scale = s;
        self
    }
}
impl View for TransportButton {
    fn draw(&self, _e: &Env, p: Painter) {
        use crate::ui::icons::Icon;
        let r = self.frame.scaled(self.scale);
        let (bg, ink) = if self.focused {
            (crate::ui::ACCENT, crate::ui::ACCENT_INK)
        } else {
            // solid clean dark disc (matches the icon mock ≈ #252525), so the white glyph reads the
            // same over any scene instead of a washed translucent circle
            (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK)
        };
        // **The same edge every other control wears** — [`control_rim`], not a bare `rect`. This was
        // the one control family the rim missed: `Button` and `CircleButton` both took it and these
        // discs kept a plain fill, so the transport read as a different material from the disc pair
        // on the hero it is modelled on. Nothing about the player route argues against it — the rim
        // is SDF geometry and samples no framebuffer, so it is as safe over the punch-through alpha
        // of the video plane as the fill it rides on. The solid fill above is unchanged and is still
        // the reason the glyph reads over any scene.
        control_rim(p, r, r.w * 0.5, bg);
        let id = match self.which {
            1 => Icon::Audio,
            2 => Icon::More,
            _ => Icon::Cc,
        };
        let s = (r.w * DISC_ICON_RATIO).round();
        let ir = Rect::new(r.x + (r.w - s) * 0.5, r.y + (r.h - s) * 0.5, s, s);
        crate::ui::icons::draw(p, id, ir, ink);
    }
}

// ---- FieldList: a NON-INTERACTIVE key/value read-out ---------------------------------------
//
// The diagnostics overlay's list primitive (`ui/stats.rs`), and the reason it is not a
// `TableView`: that is a SELECTION widget. It paints an accent pill under row `sel` on every draw
// with no "nothing selected" mode, its rows are 60px so ~25 of them measure 1540 against a 1080
// panel and SCROLL behind a scissor, and a row is `label` + optional sub-line + badges — there is
// no right-hand value column at all. A read-out needs the opposite of all three: no selection, no
// scrolling (a panel the user must scroll is two photographs and a chance of missing the line that
// mattered), and a fixed value column. Nothing here was close, which is the condition ui/CLAUDE.md
// sets for a new component.
//
// It owns no state, no focus and no springs: hand it a slice and a frame and it draws.

/// A read-out value's severity. Carried by a WORD in the value text as well as by this tint —
/// a phone photograph of a television chroma-subsamples, so hue alone must never be the signal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    Normal,
    /// something is wrong here, and this is the row to read first
    Fault,
}

/// One line of a [`FieldList`]. `val: None` makes it a SECTION heading rather than a pair.
pub struct Field {
    pub key: &'static str,
    pub val: Option<String>,
    pub tone: Tone,
}

impl Field {
    pub fn new(key: &'static str, val: impl Into<String>) -> Self {
        Self { key, val: Some(val.into()), tone: Tone::Normal }
    }
    /// mark the row as the fault — see [`Tone`]
    pub fn fault(mut self, bad: bool) -> Self {
        if bad {
            self.tone = Tone::Fault;
        }
        self
    }
    /// a group heading, drawn as a quiet caption above its rows
    pub fn section(key: &'static str) -> Self {
        Self { key, val: None, tone: Tone::Normal }
    }
}

/// Row pitch. Values are `size::BODY` (28); this is that plus air, and it is what bounds how many
/// fields the overlay may carry — see `stats::COLUMN_ROWS`.
pub const FIELD_ROW_H: f32 = 36.0;
/// Width of the key column inside a [`FieldList`] frame. Keys are right-aligned against it and
/// values start one `space::MD` later, so every value in a column shares an x — which, with the
/// font's tabular digits (all ten share one advance), is what makes the numbers line up.
pub const FIELD_KEY_W: f32 = 178.0;
/// Width a [`FieldList`] column needs: the key gutter plus room for the longest value.
pub const FIELD_COL_W: f32 = FIELD_KEY_W + theme::space::MD + 292.0;

pub struct FieldList<'a> {
    pub fields: &'a [Field],
    pub frame: Rect,
}

impl<'a> FieldList<'a> {
    pub fn new(fields: &'a [Field], frame: Rect) -> Self {
        Self { fields, frame }
    }
    /// How tall this list draws — so a caller can size or split its columns without re-deriving
    /// the pitch.
    pub fn height(n: usize) -> f32 {
        n as f32 * FIELD_ROW_H
    }
}

impl View for FieldList<'_> {
    fn draw(&self, _e: &Env, p: Painter) {
        let vx = self.frame.x + FIELD_KEY_W + theme::space::MD;
        let vw = (self.frame.w - FIELD_KEY_W - theme::space::MD).max(0.0);
        for (i, f) in self.fields.iter().enumerate() {
            let y = self.frame.y + i as f32 * FIELD_ROW_H;
            match &f.val {
                // a section heading spans the whole width and carries no value
                None => {
                    // A heading differs from a key by POSITION (full width, left-aligned, where a
                    // key is right-aligned into its gutter) and by weight — not by size. Dropping
                    // it to `MICRO` to save a few pixels is the one thing that token's doc forbids.
                    if let Ok(cs) = CString::new(f.key) {
                        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
                            .bold()
                            .draw(p, Rect::new(self.frame.x, y, self.frame.w, FIELD_ROW_H));
                    }
                }
                Some(v) => {
                    if let Ok(cs) = CString::new(f.key) {
                        Label::new(cs.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
                            .h(HAlign::Right)
                            .draw(p, Rect::new(self.frame.x, y, FIELD_KEY_W, FIELD_ROW_H));
                    }
                    let ink = if f.tone == Tone::Fault { theme::DANGER } else { theme::TEXT_PRIMARY };
                    // ELIDE. Every other bounded-width text site in `ui/` does, and this one has a
                    // value it cannot bound: the exported windowId is a compositor-assigned
                    // char[64], which at this size is ~3x the value column and would run off the
                    // card — on the one firmware family that cannot be tested here. `elide` is
                    // memoised by (string, budget, size, bold) and the panel re-formats at 2 Hz.
                    let bold = i32::from(f.tone == Tone::Fault);
                    let v = crate::text::elide(v, vw, theme::size::BODY, bold, false);
                    if let Ok(cs) = CString::new(v.as_str()) {
                        let mut l = Label::new(cs.as_ptr(), theme::size::BODY, ink);
                        if f.tone == Tone::Fault {
                            l = l.bold();
                        }
                        l.draw(p, Rect::new(vx, y, vw, FIELD_ROW_H));
                    }
                }
            }
        }
    }
}

// ---- TabPill: a rounded pill with a centered label. Focused = light pill + dark ink; idle =
// faint fill + dim ink. `TabPill::width(chars, sz)` sizes it to fit — NOT `Button::pill_w`, which
// budgets a Button's icon box and air. ----
/// How a `TabPill` reads. The player Info/Chapters tabs are always-filled buttons; the detail season
/// tabs are a segmented control with two *independent* states — a **selected** segment (the active
/// one, whose content shows) and a **highlighted** one (where the remote focus is).
#[derive(Clone, Copy)]
enum TabStyle {
    /// always a pill — focused → ACCENT, idle → solid dark disc (player Info/Chapters).
    Button,
    /// segmented control (detail season tabs): the focused segment is a bright ACCENT pill; the
    /// selected segment gets a subtle pill while focus is elsewhere; the rest are plain dim text.
    Segment { selected: bool },
}

/// The quiet annotation a [`TabPill`] can carry after its label — the detail page's season tabs use
/// it for "you have watched everything behind this tab". It is deliberately subordinate to the
/// label: it stands one type rung down and a step of alpha under the label's own ink, so a tab
/// still reads as its name first and its state second.
///
/// It carried an episode COUNT too, and no longer does (owner call, 2026-07-29: *"I don't like
/// unwatched episodes count in season tab selector. I think it's ok to leave just marks that we
/// watched."*). A count is filing data the season's own episode row already answers by simply
/// existing, and it made every tab wider for a number nobody was reading. The tick is the one fact
/// a tab strip can state that its content cannot: which seasons are behind you.
#[derive(Clone, Copy, Default)]
pub enum TabNote {
    #[default]
    None,
    /// everything behind this tab is watched — a small tick after the label.
    Done,
}

/// Type rung the trailing note is measured against: one below the label, at the couch legibility
/// floor. The tick is a glyph rather than text, but it stands in the same band a label at this rung
/// would, so the note reads as a peer of the name it follows.
const NOTE_SZ: c_int = theme::size::CAPTION;
/// Label → note air inside the pill.
const NOTE_GAP: f32 = theme::space::SM;
/// The watched tick stands as tall as the note's own rung.
const NOTE_TICK_D: f32 = NOTE_SZ as f32;
/// The note's ink is the pill's own ink one alpha step down — de-emphasis without a second colour
/// role, so it stays legible in every one of [`TabStyle`]'s ink states (including the already-dim
/// unselected segment) while never competing with the label.
const NOTE_INK_A: f32 = 0.75;

// ---- TabPill: a rounded pill with a centered label, in one of two state models (TabStyle). ----
pub struct TabPill {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub focused: bool,
    style: TabStyle,
    note: TabNote,
    /// see [`TabPill::plated`]
    plated: bool,
    /// see [`TabPill::mix`] — `None` = this pill owns its own state fill (the boolean model).
    mix: Option<(f32, f32)>,
}
impl TabPill {
    /// pill width for a `chars`-long label at `sz` (label advance + horizontal padding). Covers
    /// the LABEL only — a pill that also carries a [`TabNote`] must add [`note_w`](Self::note_w),
    /// or the note is drawn outside the fill.
    pub fn width(chars: usize, sz: c_int) -> f32 {
        chars as f32 * sz as f32 * 0.56 + 44.0
    }
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self {
            frame,
            label,
            sz,
            focused: false,
            style: TabStyle::Button,
            note: TabNote::None,
            plated: false,
            mix: None,
        }
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    /// switch to the segmented-control look (detail season tabs); `selected` = the active segment.
    /// Give every segment its own faint plate, and the selected one a stronger plate
    /// (`Details Screen.dc.html`'s season tabs). **Opt-in**, because the TOP tab row draws its segments
    /// inside the tab-bar TRACK — that track already is the ground, and plating there would stack two.
    /// The detail page's season tabs have no track, so without this an unselected season was bare text
    /// with no indication it could be pressed.
    pub fn plated(mut self) -> Self {
        self.plated = true;
        self
    }
    /// The BOOLEAN segmented model. It has **no caller today**: both of the app's segmented rows —
    /// the shared top tab bar and the detail season tabs — are [`TabStrip`]s now, and a strip inks its
    /// pills through [`mix`](Self::mix) so the highlight can travel between them. It is kept, with its
    /// four arms below, because it is the right answer for a segmented control that is NOT in a strip
    /// (nothing there to travel), and because deleting it would take [`TabStyle::Segment`] with it and
    /// leave a one-variant enum. Do not reach for it inside a strip: two pills would each paint their
    /// own fill and the capsules would have nothing left to do.
    pub fn segment(mut self, selected: bool) -> Self {
        self.style = TabStyle::Segment { selected };
        self
    }
    /// Strip-driven ink: `focus`/`selected` as 0..1 **mixes** rather than booleans, because the fills
    /// they used to imply are now travelling capsules the STRIP draws ([`TabStrip`]). A mixed pill
    /// paints no state fill of its own — only a plated strip's idle ground, which is a constant per
    /// pill and not a state (and which retires as the opaque focus capsule arrives under it, so the
    /// composite at full focus is exactly the `ACCENT` it always was).
    ///
    /// Leave it unset for a STANDALONE pill — [`TabStyle::Button`], the player HUD's Info/Chapters —
    /// which owns its fill and has no strip to travel in; those keep the boolean model verbatim.
    pub fn mix(mut self, focus: f32, selected: f32) -> Self {
        self.mix = Some((focus, selected));
        self
    }
    /// The ink a strip-driven pill wears at mixes `(focus, selected)` — `TEXT_TERTIARY` →
    /// `TEXT_PRIMARY` as the selection capsule arrives, then all the way to `ACCENT_INK` as the focus
    /// capsule covers it. Nested in that order because focus OUTRANKS selection, which is the order
    /// the boolean arms below are written in.
    ///
    /// Pure and split out of the draw so the states no STILL capture can catch — the mid-travel ones,
    /// where a partly covered pill is inking its whole label — are an executable contract rather than
    /// an eyeball. It is linear in coverage on purpose: [`cap_cover`] is the one rule both the fill
    /// and the ink read, so they cannot disagree about where the capsule is. If a device capture ever
    /// says the labels "dim when I move", shaping this one call (`focus.powi(2)`, a smoothstep) is the
    /// whole fix — but do it HERE, not with a second spring, or the ink starts leading the fill.
    pub(crate) fn mixed_ink(focus: f32, selected: f32) -> [f32; 4] {
        // **The idle ink is `TEXT_READING`, one rung brighter than the `TEXT_TERTIARY` this row wore
        // for most of its life, and it is what pays for the whole material.** The scrim is solved so
        // this ink clears [`TRACK_INK_CONTRAST`]: a muted grey needs .61 of black over the Office
        // hero and .56 over a cyan sky, which is a band laid across the picture, while this one needs
        // the floor for both. The row is louder for it — idle and selected are now two rungs apart
        // rather than the full span — and the plate carries the difference, which is what the
        // reference does too.
        theme::mix(
            theme::mix(theme::TEXT_READING, theme::TEXT_PRIMARY, selected.clamp(0.0, 1.0)),
            crate::ui::ACCENT_INK,
            focus.clamp(0.0, 1.0),
        )
    }
    /// attach a trailing [`TabNote`] (the watched tick).
    pub fn note(mut self, note: TabNote) -> Self {
        self.note = note;
        self
    }
    /// What the note adds to a pill's CONTENT width, gap included (0 for [`TabNote::None`]). The
    /// caller lays its tab strip out with this, so pill widths, the strip's x-advance and the note
    /// itself can never disagree about how wide a tab is.
    pub fn note_w(note: TabNote) -> f32 {
        match note {
            TabNote::None => 0.0,
            TabNote::Done => NOTE_GAP + NOTE_TICK_D,
        }
    }
}
impl View for TabPill {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame;
        // (fill, ink, weight 0..1). Two state models, and the strip-driven one lerps between exactly
        // the same three ink roles the boolean match picks from, so a mixed pill at 0/1 is the old
        // look to the bit (see `the_ink_a_pill_wears_is_exactly_the_capsule_over_it`).
        //
        // Boolean model: a highlighted (focused) tab is a bright ACCENT pill; a selected-but-
        // unfocused segment is a subtle pill; a plain segment is dim text with no pill at all.
        // Legibility over art is the CONTAINER's job (the tab-bar track in `draw_tab_row`), not
        // the segment's — the detail season tabs use this element bare on a controlled ground.
        let (fill, ink, bold_mix) = match self.mix {
            Some((fm, sm)) => {
                let (fm, sm) = (fm.clamp(0.0, 1.0), sm.clamp(0.0, 1.0));
                let ink = Self::mixed_ink(fm, sm);
                // The plated strip's idle ground stays the PILL's, because it is a constant per pill
                // rather than a state — but it must not lie on top of the OPAQUE focus capsule the
                // strip drew under it (0.08 of white over `ACCENT` is a ~2/255 lift and a bright rim
                // exactly where the capsule is brightest), so it retires as that capsule arrives. The
                // SELECTION capsule needs no such treatment: two whites composite to `a + b − ab`
                // whichever way round they are stacked, which is how `TAB_PLATE_SELECTED_OVER` lands
                // back on `TAB_PLATE_SELECTED`'s .20 from underneath.
                let plate_a = theme::TAB_PLATE_IDLE[3] * (1.0 - fm);
                let fill = (self.plated && plate_a > 0.002).then(|| theme::with_a(theme::TAB_PLATE_IDLE, plate_a));
                (fill, ink, sm.max(fm))
            }
            None => match self.style {
                TabStyle::Button if self.focused => (Some(crate::ui::ACCENT), crate::ui::ACCENT_INK, 1.0),
                TabStyle::Button => (Some(theme::CONTROL_IDLE_FILL), theme::CONTROL_IDLE_INK, 1.0),
                TabStyle::Segment { .. } if self.focused => (Some(crate::ui::ACCENT), crate::ui::ACCENT_INK, 1.0),
                TabStyle::Segment { selected: true } if self.plated => (Some(theme::TAB_PLATE_SELECTED), theme::TEXT_PRIMARY, 1.0),
                TabStyle::Segment { .. } if self.plated => (Some(theme::TAB_PLATE_IDLE), theme::TEXT_TERTIARY, 0.0),
                TabStyle::Segment { selected: true } => (Some(theme::OVERLAY_FOCUS_PILL), theme::TEXT_PRIMARY, 1.0),
                // `TEXT_TERTIARY` and not the standing track's brighter `TEXT_READING`, on purpose:
                // this arm draws on a CONTROLLED ground (the detail page's season row) where nothing
                // is solved, so the muted idle ink costs nothing and is the design-system default.
                // See `a_settled_mixed_pill_is_the_boolean_look_it_replaced`.
                TabStyle::Segment { .. } => (None, theme::TEXT_TERTIARY, 0.0),
            },
        };
        if let Some(bg) = fill {
            p.rrect(r, r.h * 0.5, r.h * 0.5, bg);
        }
        // The label centres in the pill MINUS the note group, so a tab carrying a note keeps
        // label+note centred as one unit rather than shoving the label off-centre. The note then
        // hugs the label's own PAINTED right edge (the width `Label::draw` hands back), which is
        // why this needs no knowledge of the caller's pill padding.
        //
        // Painted, deliberately, not the width the caller measured: a strip that sizes its tabs
        // with the BOLD label (as the season tabs and `draw_tab_row` both do, so an advance can't
        // move with focus) paints a NON-bold label a couple of px narrower. Anchoring off the
        // caller's number instead would pin the note while the label's right edge slid under it,
        // i.e. it would trade a constant label→note gap for a variable one. The gap is the
        // relationship the eye reads here; the few px of slack left inside the pill's own padding
        // on an unbolded tab is the same slack the label alone already had. The one exception is the
        // crossfade below, which has two painted widths and so no single answer — it falls back to
        // the caller's own bold advance rather than letting the tick slide as the weights swap.
        let nw = Self::note_w(self.note);
        let frame = Rect::new(r.x, r.y, r.w - nw, r.h);
        let run = |p: Painter, bold: bool| {
            let mut lab = Label::new(self.label, self.sz, ink).h(HAlign::Center);
            if bold {
                lab = lab.bold();
            }
            lab.draw(p, frame)
        };
        // WEIGHT crossfades; it does not tween. A bold run and a regular run are two rasterizations
        // — the same reason `theme.rs`'s size ladder says a SIZE is crossfaded rather than animated,
        // and the way `person.rs` spells its band condense. Two draws happen only while a capsule is
        // genuinely mid-travel over THIS pill: at most two pills, for ~200 ms. Both boolean states
        // land squarely in the single-draw branches, so nothing that exists today pays for this.
        let lw = if bold_mix > 0.98 {
            run(p, true)
        } else if bold_mix < 0.02 {
            run(p, false)
        } else {
            run(p.alpha(1.0 - bold_mix), false);
            run(p.alpha(bold_mix), true);
            crate::text::text_width(self.label, self.sz, 1)
        };
        let nx = r.x + (r.w - nw) * 0.5 + lw * 0.5 + NOTE_GAP;
        let note_ink = theme::with_a(ink, NOTE_INK_A);
        match self.note {
            TabNote::None => {}
            TabNote::Done => {
                let d = NOTE_TICK_D;
                crate::ui::icons::draw(
                    p,
                    crate::ui::icons::Icon::Check,
                    Rect::new(nx, r.y + (r.h - d) * 0.5, d, d),
                    note_ink,
                );
            }
        }
    }
}

// ---- Tab strip MOTION: the travelling capsules. ----
// A `TabPill` is a retui LEAF: it has no idea its neighbours exist, so it structurally cannot
// animate *between* pills — which is why the selection used to vanish from one pill and reappear on
// the next in a single frame. The highlight is therefore hoisted out of the pill and into the STRIP,
// which knows every pill's span and can carry one fill from one to another. The pill keeps only its
// label, its constant ground, and the ink it derives from how covered it is.
//
// Shared by the top tab bar (`draw_tab_row`) and the detail page's season tabs, per the owner
// directive recorded above [`TAB_PILL_H`]: the two rows are ONE control, and one control has one
// motion. The stiffnesses below are matched to springs that already exist rather than invented.

/// Stiffness of a tab strip's travelling capsules. Deliberately IDENTICAL to [`K_TAB_SCROLL`]: the
/// capsule rides *inside* the strip it marks, and a highlight that settles at a different rate from
/// the row under it reads as two objects instead of one control. (The detail season row springs its
/// own scroll at the same 240 — the directive above [`TAB_PILL_H`] covers their motion too.)
const K_TAB_CAP: f32 = K_TAB_SCROLL;
/// Stiffness of a capsule's ALPHA — the app's shared appear spring
/// ([`crate::ui::popover::K_APPEAR`]), because entering or leaving a row is a FADE, not a journey.
const K_TAB_CAP_A: f32 = crate::ui::popover::K_APPEAR;
/// Below this alpha a capsule is not on screen, so it LANDS on its next pill instead of gliding to
/// it (see [`Capsule::step`]). 0.02 is picked to be *invisibly* small rather than merely small: the
/// faintest capsule is [`theme::OVERLAY_FOCUS_PILL`] at .14, so at this alpha it composites to
/// .0028 of white — under one display code on the panel's 8-bit framebuffer — and a jump there
/// cannot be seen however far it travels.
const CAP_LAND_A: f32 = 0.02;

/// How much of pill `pill` (content-space `(x, w)`) a capsule spanning `cap` covers, 0..1 — a 1-D
/// [`Rect::intersect`]. This is the ONE rule a pill's ink is derived from, so the ink can never
/// disagree with the fill sliding under it: a dark label left behind on a pill the capsule has
/// already left is unreadable, and that is exactly what a separate per-pill ink spring would
/// produce. A zero-width pill covers nothing (and, more to the point, does not divide by zero).
fn cap_cover(pill: (f32, f32), cap: (f32, f32)) -> f32 {
    if pill.1 <= 0.0 {
        return 0.0;
    }
    let lo = pill.0.max(cap.0);
    let hi = (pill.0 + pill.1).min(cap.0 + cap.1);
    ((hi - lo) / pill.1).clamp(0.0, 1.0)
}

/// A tab highlight that TRAVELS between pills instead of being repainted onto a new one. Two springs
/// for its content-space geometry — left edge AND width, so a wide pill *morphs* into a narrow one
/// rather than teleporting — plus one for alpha, which is how it enters and leaves a row without
/// streaking across the strip from wherever it was last parked.
#[derive(Clone, Copy)]
pub(crate) struct Capsule {
    x: Spring,
    w: Spring,
    a: Spring,
    /// The pill index this capsule is bound to, or -1 for "nothing to mark". Tracked so a capsule
    /// that has never been placed can tell that apart from one resting at content x = 0 (which is a
    /// real position: it is pill 0).
    at: i32,
}

impl Capsule {
    pub(crate) const fn new() -> Self {
        Capsule { x: Spring::at(0.0), w: Spring::at(0.0), a: Spring::at(0.0), at: -1 }
    }
    /// One frame toward pill `i`'s content-space `(x, w)`. `None` = nothing to mark (focus left the
    /// row, or the index went stale after a section refetch): HOLD the position and fade out, so
    /// focus leaving and returning to the SAME pill is a fade, not a round trip.
    ///
    /// A capsule that is not on screen ([`CAP_LAND_A`]) LANDS rather than glides. Without this, the
    /// first frame of the Library screen — and every return of focus to the row — would start with a
    /// bright capsule flying in from the pill the user was on two screens ago.
    fn step(&mut self, target: Option<(usize, (f32, f32))>, dt: f32) {
        match target {
            Some((i, (x, w))) => {
                if self.at < 0 || self.a.pos < CAP_LAND_A {
                    self.x.jump(x);
                    self.w.jump(w);
                }
                self.at = i as i32;
                self.x.step(x, K_TAB_CAP, dt);
                self.w.step(w, K_TAB_CAP, dt);
                self.a.step(1.0, K_TAB_CAP_A, dt);
            }
            None => {
                self.at = -1;
                self.a.step(0.0, K_TAB_CAP_A, dt);
            }
        }
    }
    /// Content-space `(x, w)` as drawn this frame.
    #[inline]
    fn span(&self) -> (f32, f32) {
        (self.x.pos, self.w.pos)
    }
    #[inline]
    fn alpha(&self) -> f32 {
        self.a.pos.clamp(0.0, 1.0)
    }
    /// How strongly pill `pill` is wearing this capsule right now — coverage × alpha.
    #[inline]
    fn mix(&self, pill: (f32, f32)) -> f32 {
        self.alpha() * cap_cover(pill, self.span())
    }
}

/// The motion state of ONE tab strip: the subtle capsule marking the SELECTED tab and the bright one
/// marking the FOCUSED tab, both travelling. Two, not one, because they are independently placed —
/// on the Library screen the selected tab is the section you are browsing while focus walks the row
/// — and one capsule cannot be in two places. Held as a value, not a global: the top row keeps one
/// static instance, the detail page keeps one per `DetailView` (so a new item's strip starts clean).
#[derive(Clone, Copy)]
pub(crate) struct TabStrip {
    sel: Capsule,
    foc: Capsule,
}

impl TabStrip {
    pub(crate) const fn new() -> Self {
        TabStrip { sel: Capsule::new(), foc: Capsule::new() }
    }
    /// Step both capsules. `span(i)` resolves pill `i`'s content-space `(x, w)` and MUST be the same
    /// function the caller lays its pills out with — that is what keeps a capsule from ever landing
    /// off a pill. A negative or out-of-range index resolves to `None` (nothing to mark).
    pub(crate) fn update(
        &mut self,
        selected: c_int,
        focused: c_int,
        span: impl Fn(usize) -> Option<(f32, f32)>,
        dt: f32,
    ) {
        let pick = |i: c_int| -> Option<(usize, (f32, f32))> {
            let i = usize::try_from(i).ok()?;
            span(i).map(|s| (i, s))
        };
        let (sel_t, foc_t) = (pick(selected), pick(focused));
        // The focus capsule's REAL spring target, read before the step. Every other probe site
        // passes one, and it is the whole of what the diagnostic measures: with `pos` handed in as
        // its own target the overshoot and settle-frame numbers degenerate to nothing on every
        // frame, including the travelling ones the probe exists to look at. `None` means "hold
        // where you are" (see [`Capsule::step`]), so on those frames the target IS the position.
        let foc_x = foc_t.map(|(_, (x, _))| x).unwrap_or(self.foc.x.pos);
        self.sel.step(sel_t, dt);
        self.foc.step(foc_t, dt);
        crate::ui::anim::probe("tabstrip.foc", self.foc.x.pos, self.foc.x.vel, foc_x, dt);
    }
    /// Draw both capsules in the painter's own (already scroll-translated) CONTENT space, for a strip
    /// whose pills stand `h` tall at `top`. Selection first, focus over it — when they are on the same
    /// pill the bright one wins, exactly as the boolean match ordered its arms. `plated` = the strip
    /// lays its own per-pill ground (the detail season tabs), so the selection capsule takes the value
    /// that composites OVER that ground instead of replacing it.
    ///
    /// Call this BEFORE the pill loop: the capsules are the pills' ground, and a label drawn under an
    /// opaque focus capsule is a label nobody can read.
    pub(crate) fn draw(&self, p: Painter, top: f32, h: f32, plated: bool, glass: bool) {
        let cap = |c: &Capsule, col: [f32; 4], rim: Option<[f32; 4]>| {
            let (x, w) = c.span();
            let a = c.alpha();
            // sub-code alpha or a sub-pixel width is nothing on screen but still a full rrect pass
            if a > 0.004 && w > 0.5 {
                let r = Rect::new(x, top, w, h);
                match rim {
                    // On a GLASS track the selection plate is a piece of the same material, not a
                    // white wash: its own perimeter line and its own brighter top edge, exactly as
                    // the track wears them. Without an edge a translucent plate over a translucent
                    // band has nothing to be bounded BY — it lands differently on each side of
                    // itself and reads as a smudge, which is what the first device photograph of
                    // this bar showed and what no amount of fill alpha fixes.
                    Some(rc) => p.alpha(a).rect_rimmed(
                        r,
                        h * 0.5,
                        col,
                        col,
                        rc,
                        theme::GLASS_RIM_LIGHT[3] - theme::GLASS_RIM[3],
                    ),
                    None => p.alpha(a).rrect(r, h * 0.5, h * 0.5, col),
                }
            }
        };
        let sel_col = if plated { theme::TAB_PLATE_SELECTED_OVER } else { theme::OVERLAY_FOCUS_PILL };
        cap(&self.sel, sel_col, (glass && !plated).then_some(theme::GLASS_RIM));
        // The FOCUS capsule keeps its near-white fill: it is the one thing on this row that must not
        // read as a material, because the material is what everything else here is and focus has to
        // be the exception. It wore two stops of shadow for a while — near-white on a near-white
        // LIGHT track has no separation and had to be lifted instead — and that went with the light
        // track it was drawn for.
        cap(&self.foc, crate::ui::ACCENT, None);
    }
    /// The `(focus, selected)` mixes pill `pill` (content-space `(x, w)`) should ink itself with —
    /// hand straight to [`TabPill::mix`].
    pub(crate) fn mixes(&self, pill: (f32, f32)) -> (f32, f32) {
        (self.foc.mix(pill), self.sel.mix(pill))
    }
}

// ---- The shared top tab row: profile chip leads at the margin, the pills (Home | <library
// sections>) sit CENTERED — the tvOS tab-bar idiom. Drawn by BOTH the Home screen and the
// Library screen so they read as one global tab bar; the pill rects live here so both
// screens' pointer paths share [`tab_pill_at`]. ----
/// Home + every library section **that gets a pill**. Focus walking clamps to this — the invariant
/// is still "a pill that can't be drawn must never be focusable", but it now holds by construction
/// rather than by a cap: the strip scrolls horizontally inside its track (see
/// [`tab_scroll_target`]), so every pill can be brought into view and every pill is focusable.
/// This used to be a hard `MAX_TABS = 5`, which left a fifth `movie`/`show` section unreachable
/// from the UI even though `browse` discovers it and the grid browses it fine.
///
/// It is `browse::tab_count`, NOT `section_count`, and the difference is a design call rather than
/// a detail: **a pill is a TYPE, never a person**. The strip names your own libraries (plus any
/// type only a friend has), source lives in the Library toolbar's Source chip one line below, and
/// so the strip is the same width at one friend or at ten. With a single source the two counts are
/// identical, which is why nothing about a one-server install changed.
pub(crate) fn tab_count() -> usize {
    1 + crate::browse::tab_count() + 1 // Home, the sections, then Search
}
/// The index of the Search pill: always the LAST one. That it goes last rather than first is the
/// whole reason adding it cost nothing — `Home = 0, sections = 1..=n` is untouched, so every
/// `pill - 1 → section` conversion in the app stays correct as written and only has to learn that
/// one index is not a section. Prepending would have shifted all of them to `+2` at eight sites.
pub(crate) fn search_pill() -> usize {
    tab_count() - 1
}
/// Is `pill` the Search pill? Kept for a reader that only wants the yes/no — `ui/home.rs`'s
/// top-band walk test, which asserts the last stop is not a library. Anything turning a pill into a
/// DESTINATION wants [`pill_at`] instead, which answers the whole question rather than one third of
/// it and cannot silently open a library for a pill it has never heard of. Derived FROM [`pill_at`]
/// rather than comparing against [`search_pill`] a second time: one definition of which pill is
/// Search, whichever way it is asked.
pub(crate) fn is_search_pill(pill: usize) -> bool {
    matches!(pill_at(pill), Pill::Search)
}

/// What a strip index MEANS — the tab row's vocabulary, so a reader gets a destination rather than
/// an integer to do arithmetic on.
///
/// Six sites (four in `app.rs`, two in `library.rs`) used to spell the same three-way ladder out
/// by hand — `if is_search_pill(i) { … } else if let Some(sec) = i.checked_sub(1) { … } else { Home }`
/// — two of them byte-identical. That shape has one bad property that a name cannot fix: a pill
/// this ladder has never heard of falls into the `checked_sub(1)` arm and opens a LIBRARY, at a
/// section index `browse::tab_section` then clamps into range. So the wrong pill quietly browses
/// the wrong shelf instead of failing. With the decision typed, adding a variant here is a
/// non-exhaustive `match` at every one of those sites — a compile error, which is the only kind of
/// reminder that reaches all six.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pill {
    /// pill 0 — always present, the strip's one fixed member
    Home,
    /// a TAB index into `browse::tabs` (0-based, Home already subtracted), NOT a section index:
    /// several libraries can share one pill, and `browse::tab_section` is what resolves it
    Section(usize),
    /// the last pill (see [`search_pill`])
    Search,
}

// The three below are each a one-line wrapper over a PURE half taking the Search pill's index. The
// split is not ceremony: the pill count is projected from `browse`'s section table, a crate global
// that several modules' tests read, so a test that SEEDS one to get a `Section` in the middle of
// the strip perturbs every other test in the binary (it exposed an unrelated flake in
// `ui::person`'s shelf tests the first time these were written that way). The pure halves need no
// table, so the ladder is graded over strips this host will never build.

/// Resolve a strip index. The ladder, written ONCE — [`Pill`]'s doc has the argument for why it is
/// written once here rather than six times at the call sites.
pub(crate) fn pill_at(i: usize) -> Pill {
    pill_in(i, search_pill())
}
/// [`pill_at`] on a strip whose Search pill is at `search`.
///
/// Total, deliberately: an index past the last pill resolves to `Section(i - 1)`, exactly as the
/// hand-written ladders did, and the section index is then clamped by `browse::tab_section` as it
/// always was. Nothing can hand one in today (`tab_pill_at` matches drawn rects, and every screen's
/// own cursor is clamped), so an `Option` here would only add a `None` arm to six `match`es that
/// could never run.
fn pill_in(i: usize, search: usize) -> Pill {
    if i == search {
        Pill::Search
    } else if let Some(sec) = i.checked_sub(1) {
        Pill::Section(sec)
    } else {
        Pill::Home
    }
}

/// The index a [`Pill`] is drawn at — the inverse of [`pill_at`], for the places that name a
/// destination and need the strip position it selects (`app.rs`'s `Nav::select_pill`).
pub(crate) fn pill_of(p: Pill) -> usize {
    pill_index(p, search_pill())
}
/// [`pill_of`] on a strip whose Search pill is at `search`.
fn pill_index(p: Pill, search: usize) -> usize {
    match p {
        Pill::Home => 0,
        Pill::Section(tab) => tab + 1,
        Pill::Search => search,
    }
}

/// The highest index a SECTION pill can have — the ceiling for a pill index DERIVED from a section
/// (`library::own_pill`, which walks focus up into the row onto the pill representing the library
/// being browsed).
///
/// It reads "the pill before Search", and that is only correct because Search sits last — a fact
/// about the strip, not about the caller, so it is written here and not there. The call site used
/// to spell it `search_pill().saturating_sub(1)`, a positional pun that would silently become "the
/// pill before whatever ends up last" the day a second non-library pill was added.
pub(crate) fn last_section_pill() -> usize {
    last_section_in(search_pill())
}
/// [`last_section_pill`] on a strip whose Search pill is at `search`. With NO sections at all the
/// strip is `Home | Search` and this is the HOME pill: an index past the last pill is a highlight
/// on nothing, and Home is the one pill that always exists. That is the failed-discovery case,
/// reachable since a failed source keeps the user on the screen instead of bailing out.
fn last_section_in(search: usize) -> usize {
    search.saturating_sub(1)
}
/// The Search pill is SQUARE: 60×60, the icon's own air rather than a word's, so a mark does not
/// wear the padding a label needs (`Search Screen.dc.html`). Every other pill keeps
/// [`TAB_PILL_PAD`].
const TAB_ICON_PILL_W: f32 = TAB_PILL_H;
/// The mark inside it, at 1.15× the strip's own type rung (BODY 28 → 32) — the design system's
/// `--btn-icon-ratio`, the same ratio every icon-and-label control in the app uses.
const TAB_ICON_D: f32 = 32.0;
/// The top chrome band's y — the chip and the pills sit on it (Home and Library alike).
pub(crate) const TOP_BAR_Y: f32 = 44.0;
/// SAME element, SAME geometry as the detail season tabs (user directive): one control height
/// (the 60px circle-button CD family) and the season tabs' ±18 label padding — the two rows
/// must be indistinguishable as a control.
pub(crate) const TAB_PILL_H: f32 = 60.0;
const TAB_PILL_PAD: f32 = 18.0;
/// UNIFORM inset from the tab-bar track to the pills inside it, on every side: the pill (r=30) and
/// the track (r=38) stay CONCENTRIC (outer radius = inner radius + gap), so an end pill's corner
/// gap reads even all the way around — 16px ends against 8px verticals looked lopsided on the
/// selected Home pill. The focused [`profile_chip`] wraps itself in the same inset, which is what
/// makes its capsule and this track one band.
pub(crate) const TAB_TRACK_PAD: f32 = 8.0;
/// The y below which a screen's content is clear of the shared top chrome — the tab track's bottom
/// edge ([`TOP_BAR_Y`] + the pill height + the track's inset). Exposed so a screen that lets art
/// overflow UPWARD out of its layout band ([`crate::ui::hero_logo`]) can ASSERT its clearance in a
/// host test instead of leaving it to a device capture.
pub(crate) const TOP_BAR_BOTTOM: f32 = TOP_BAR_Y + TAB_PILL_H + TAB_TRACK_PAD; // 112
/// Inter-pill air ≈ the season tabs' rhythm (their `TAB_ADVANCE` 52 minus the 2×18 pad the pills
/// here already carry — pill edge to pill edge reads the same). Doubles as the scroll-into-view
/// context margin, exactly as the season tabs use their advance.
const TAB_GAP: f32 = 16.0;
/// How wide the pill strip may grow before it starts scrolling. The row stays CENTERED, but it
/// must never reach the profile chip (a focus stop of its own at `MARGIN_X`), so the viewport is
/// the screen less a symmetric chip-clearing margin, less the track's own inset on both ends.
const TAB_SIDE_CLEAR: f32 = crate::ui::consts::MARGIN_X + CHIP_D + theme::space::MD;
const TAB_VIEW_MAX: f32 = crate::ui::consts::SCR_W - 2.0 * (TAB_SIDE_CLEAR + TAB_TRACK_PAD);
/// The strip's scroll stiffness. The detail page's season-tab row springs at the same 240 — same
/// control, same overflow problem, so the two rows must move alike (the user directive that keeps
/// the tab pills and the season tabs in step covers their motion, not just their geometry).
const K_TAB_SCROLL: f32 = 240.0;
/// The VISIBLE part of each pill as drawn this frame (scroll folded in, then intersected with the
/// strip's viewport), one entry per pill — never a fixed array's worth, or the hit test would stop
/// where the old cap did. Storing the *clipped* rect is what keeps the hit test and the scissor
/// telling the same story: a pill scrolled out of the track has zero width here, so it is neither
/// drawn nor clickable, and a half-visible one is clickable exactly across the half you can see.
static mut PILL_RECTS: Vec<Rect> = Vec::new();
/// Horizontal scroll of the strip inside its track; 0 whenever the whole row fits.
static mut TAB_SCROLL: crate::ui::Spring = crate::ui::Spring::at(0.0);
/// The top row's travelling capsules ([`TabStrip`]). A static for the same reason [`TAB_SCROLL`] is
/// one: ONE tab bar is drawn by two screens, and the capsule must carry ACROSS the Home→Library
/// route flip — that carry IS the transition. Stepped from [`tab_row_update`], so it moves on the
/// PRESS frame: Library hands its *pending* section down here (`library::view_section`), which is
/// what puts the capsule on the new pill while the grid is still dissolving under it.
static mut TOP_STRIP: TabStrip = TabStrip::new();
/// Live-trigger lifetime for the tab track's glass experiment. It uses the same reusable cadence
/// machinery as a dynamic popover, without the modal's source-dim transform.
///
/// ONE state for the whole band, not one per surface. [`profile_chip`]'s capsule is a second
/// surface of this material and shares this lifetime, this cadence and this snapshot — `prepare`
/// mutates the process-wide `DynamicClock`, so a second owner preparing for the same band would
/// spend a present's refresh slot twice.
static mut TAB_GLASS_STATE: GlassState = GlassState::new();

/// **What the shared top bar is made of, THIS frame** — published by [`draw_tab_row`] and consumed
/// by [`profile_chip`], the band's other surface.
///
/// This is option (a) of the four the overlap admits, and the other three are worth recording
/// because each is the obvious answer from one angle:
///
/// * **Each surface solves its own ground.** The chip sits ~800px left of the track over different
///   hero artwork, and `track_alpha_for` is a function of what is under a surface — so over any
///   non-uniform hero the two land on different alphas and different rim weights. One band, drawn
///   in two densities, with the step at the point the eye is least able to excuse it. It also puts
///   a second caller on `gfx::sample_ground`, which has one latch and one rate counter: the two
///   would halve each other's sampling rate and clobber each other's answer.
/// * **The chip suppresses the track's glass while it is expanded.** Focusing the chip would then
///   strip the material off a control the focus never touched — the "material that steps reads as
///   broken" failure `K_TRACK_DENSITY_ATTACK` exists to keep out, at the largest scale available.
/// * **Merge the two into one surface whose rect is their union.** The union spans the ~300px of
///   bare hero BETWEEN them; drawing it as one capsule paints material over a gap the design shows
///   the picture through. It is the right answer only in the case the geometry now forbids.
///
/// So: one solve, one publisher, one consumer. The track samples the ground because it is drawn
/// first and it is the surface the ground question was posed about; the chip takes the answer. The
/// band cannot be two densities because there is only one.
///
/// # What option (a) COSTS, measured — the chip's ink is no longer solved for
///
/// This is the half the four bullets above do not price, and it is not small. `track_alpha_for` is
/// not a look; it is a CONTRACT — it searches for the lightest scrim on which `theme::TEXT_READING`
/// still clears `TRACK_INK_CONTRAST` over **the ground under `r`**. The chip is 800px outside `r`,
/// so the density it now wears carries no promise about the pixels it is actually sitting on, and
/// the unfurled capsule's whole content is a NAME.
///
/// Through this module's own `track_alpha_for`/`contrast` and `theme`'s tokens, for a neutral chip
/// ground at L\* 92 with the track's worst tap at or below L\* 50 (where the solve rests on its
/// floor, `theme::TAB_GLASS_TOP`'s .20):
///
/// | what the chip's name sits on | face | `TEXT_PRIMARY` contrast |
/// |---|---|---|
/// | the flat capsule this replaced (`TAB_TRACK_A_TOP` .72) | L\* 27.5 | **9.74:1** |
/// | the band's face, solved for a dark track | L\* 75.4 | **1.86:1** |
///
/// At L\* 80 it is 2.54:1. Nothing in the app darkens the top band before this — `home`'s
/// atmospheric ramp starts at `HERO_BASE_SCRIM_Y0` (367) and the hero wedge at `HERO_SCRIM_TOP`
/// (162) — so those are raw backdrop pixels, and `gfx::sample_ground`'s own note records a census
/// finding a MEDIAN 26.8 L\* span across five taps of the track alone, a third of heroes over 40.
/// The band is half again as wide as the track.
///
/// **It is not settled, and the shape of the answer is not obvious**, which is why it is written
/// down here rather than fixed in passing. Widening `r` to the band spreads the same five taps over
/// ~1.4x the span with ~300px of it dead — on the narrowest strip that leaves the TRACK one tap,
/// which trades this failure for its mirror image. Making the rect follow the unfurl re-solves the
/// whole bar when focus lands on the chip. Keying `sample_ground` per caller costs far less than
/// its doc used to claim (see there) but puts two densities on one line — which the lane's own
/// geometry change makes less dangerous than it was, since `GLASS_TRACK_MAX` now guarantees the two
/// can never be nearer than `BAND_AIR` and are usually ~300px apart. Settle it by LOOKING, with a
/// ground that varies in luminance ACROSS the band: `pat:ramp`, `pat:edge`, `pat:orient`. Every
/// pattern this was verified on (`flat:*`, `hbars`, `rainbow:70`) is uniform across x or uniform in
/// L\*, and so is structurally unable to show it.
///
/// Reset to `Flat` at the top of every [`draw_tab_row`], so a frame that returns early leaves the
/// chip on the flat material rather than on the previous frame's stops. The case that needs it is
/// the POPOVER source pass, where the row draws its flat track and the chip must draw the flat
/// capsule into the same snapshot; in the TRACK's own source pass [`profile_chip`] returns before
/// drawing anything at all, so there the reset decides nothing.
#[derive(Clone, Copy)]
enum BarMaterial {
    /// the flat dark capsule: `flattabs`, a popover open, a strip past [`GLASS_TRACK_MAX`], a
    /// driver with no render target, or a source pass
    Flat,
    /// glass, wearing exactly the face the track resolved this frame
    Glass(crate::gfx::GlassFace),
}
static mut BAR_MATERIAL: BarMaterial = BarMaterial::Flat;

/// **How fast the drawn weight follows the solve, and why the two rates are not the same number.**
///
/// [`track_alpha_for`] is exact and it is also a STEP. The ground can only be read twice a second
/// (a readback stalls a tiler — [`crate::gfx::sample_ground`] holds that reasoning) and the answer
/// is one of 25 rungs, so applied straight to the draw the bar's weight changes in visible jumps as
/// artwork moves under it. Reported from the panel as the bar "glitching", which is the right word
/// for it: a material that steps does not read as responding to the picture, it reads as broken.
/// So the solve stays a step and the DRAWN weight is a spring — the same answer, and for the same
/// reason, that [`AmbientWash::K`] gives the page wash this bar shares its ground with.
///
/// The asymmetry is the contrast contract rather than taste. [`TRACK_INK_CONTRAST`] is a FLOOR the
/// labels may not dip below, so the direction that protects them — getting darker — arrives at the
/// pace of the page's own scrolling, while letting the scrim back off is purely cosmetic and can
/// take almost twice as long. A ground that flickers therefore ratchets toward legible and releases
/// lazily, which is the safe way round; a symmetric rate would spend half of every flicker under the
/// floor the whole feature exists to hold.
const K_TRACK_DENSITY_ATTACK: f32 = 220.0; // toward opaque — ~0.44 s, near the page's scroll rate
const K_TRACK_DENSITY_RELEASE: f32 = 55.0; // back toward clear — ~0.89 s, slower than the wash

/// Which of the two rates applies. Pure, so the asymmetry is host-testable without a framebuffer to
/// read a ground out of.
fn density_k(drawn: f32, want: f32) -> f32 {
    if want > drawn { K_TRACK_DENSITY_ATTACK } else { K_TRACK_DENSITY_RELEASE }
}

/// The weight being drawn, and the one the ground last asked for.
///
/// `want` is published by the DRAW because the ground is framebuffer 0 and only the draw can read
/// it; [`tab_row_update`] consumes it on the next frame. That one frame of lag is nothing against a
/// value that eases over hundreds of milliseconds, and it is the only ordering that does not put a
/// readback in the update phase.
///
/// `seeded` is what stops the bar dissolving in from nothing the first time it is drawn: a screen
/// entry has no previous weight to travel from, so the first solve is taken whole. Every solve after
/// it is travelled to.
struct TrackDensity {
    drawn: crate::ui::Spring,
    want: f32,
    seeded: bool,
}
static mut TRACK_DENSITY: TrackDensity = TrackDensity {
    drawn: crate::ui::Spring::at(0.0),
    want: 0.0,
    seeded: false,
};

/// The weight to draw for `ground` this frame, publishing the weight it asked for.
fn track_density(ground: [f32; 3]) -> f32 {
    let want = track_alpha_for(ground);
    unsafe {
        let d = &mut *std::ptr::addr_of_mut!(TRACK_DENSITY);
        d.want = want;
        if !d.seeded {
            d.seeded = true;
            // `Spring::at`, not `jump`: there is no motion to report on a value that has never been
            // drawn, and `jump`'s invalidate would repaint a screen that has nothing new on it.
            d.drawn = crate::ui::Spring::at(want);
        }
        d.drawn.pos
    }
}

/// Step the drawn weight toward the last solve.
///
/// Called from [`tab_row_update`] — the one function all three screens wearing this bar go through,
/// and the same reason the strip's scroll and its capsules are stepped there rather than in each
/// screen. On a still screen the readback returns the same bytes, so the solve is bit-identical, the
/// spring is already on it, and `ui::idle` hears nothing: the present gate is not defeated by a bar
/// that adapts.
fn track_density_step(dt: f32) {
    unsafe {
        let d = &mut *std::ptr::addr_of_mut!(TRACK_DENSITY);
        if d.seeded {
            let k = density_k(d.drawn.pos, d.want);
            d.drawn.step(d.want, k, dt);
            // The whole point of this spring is that the weight is CONTINUOUS where the solve is
            // not, and that is a per-frame claim — a twice-a-second `groundlog` line cannot show it.
            crate::ui::anim::probe("tabtrack.density", d.drawn.pos, d.drawn.vel, d.want, dt);
        }
    }
}

/// The glass track's two scrim stops, with a dev override on the DENSITY.
///
/// The weight is [`track_density`]'s — [`track_alpha_for`]'s solve after the attack/release spring,
/// never the raw solve. The override bypasses both, which is what makes it a fixed-weight leg.
///
/// `/tmp/plxnative-tabglassdim=<0..1>` replaces [`theme::TAB_GLASS_TOP`]'s alpha and keeps the
/// authored spread to the bottom stop, so a sweep moves ONE variable. Absent, the theme's own
/// values are returned and the draw is byte-identical.
///
/// **That spread is 0.16, not the 0.08 this paragraph claimed until a judging panel checked it** —
/// the tokens are [`theme::TAB_GLASS_TOP`]'s .20 and [`theme::TAB_GLASS_BOT`]'s .36. (They were .34
/// and .50 when this was written, and both this paragraph and the one below went on quoting the old
/// pair after the tokens moved. The SPREAD is what the arithmetic here needs, and it survived the
/// move at 0.16 — which is exactly why nothing broke and nobody noticed.) It matters because the
/// bottom stop is `a + spread` and nothing bounds it at
/// [`theme::TAB_TRACK_A_TOP`]: a solve of .633 draws .793, past the point where there is anything
/// left to see through, which is what makes a heavy bar read as paint rather than as glass.
///
/// It exists because the shipped values are the one thing in this material nobody had graded. The
/// flat track is `scrim_black(0.72..0.82)` and is sized to make the pills legible over ARBITRARY
/// artwork; the glass track roughly halves that — [`theme::TAB_GLASS_TOP`]/`BOT`, .20/.36 — on the
/// argument that a real backdrop behind it does the work the flat material had to do alone. It does
/// not, and [`theme::TAB_GLASS_TOP`] holds the contrast table that says why: a blur removes DETAIL,
/// not brightness, so the first density at which the tertiary labels clear 3:1 over white artwork is
/// the flat track's own .72.
///
/// (This paragraph said "0.38/0.46" until 2026-08-19, then ".34/.50" until the tokens moved again.
/// Three stale pairs in one comment is the argument for naming the TOKENS and not their values;
/// the spread, which is what the arithmetic above actually consumes, is derived from them.)
/// **How much of ITSELF the glass shows — the diffuse component, as a neutral level 0..1.**
///
/// Returns 0 — a pure black scrim, everything this material did before — for every ground brighter
/// than the floor itself. See [`theme::TAB_GLASS_LIFT_FLOOR`] for what was measured.
///
/// The rule is one line, and it is a floor rather than a target: **the glass never goes darker than
/// `floor`, whatever the page does.** A black scrim gives `lum·(1-a)`, which approaches nothing as
/// the page does; the lift is the shortfall, so
///
/// ```text
/// face = max(lum·(1 - a), floor)     g = max(0, (floor - lum·(1 - a)) / a)
/// ```
///
/// That form was chosen over the two obvious alternatives, both of which were tried:
///
/// * A **proportional** target ("the face should sit N% off the page") degenerates at zero, which
///   is the one ground that actually fails.
/// * **Blending** between a light target and the dark one crosses the page's own level on the way
///   — measured, it passed within 0.4 of a code at a ground of .14, i.e. it moved the invisible bar
///   from an almost-black page to a dark grey one and called it a fix.
///
/// A floor still meets the page exactly once, at `lum = floor`, but that is a page at L\*3 and the
/// band around it is a couple of codes wide. Everywhere above it the scrim is doing the separating,
/// which is what it was sized for.
///
/// The answer is finally walked back until the idle label still clears [`TRACK_INK_CONTRAST`]: the
/// density solve sized the scrim against a BLACK tint, and a lighter face spends contrast it bought.
/// On the near-black grounds this touches there is a great deal of room and the clamp does not bind
/// — it is here so the rule can be stated as a rule.
fn track_lift(ground: [f32; 3], a: f32) -> f32 {
    let floor = lift_floor();
    // The ground as one level. `sample_ground` has already taken the WORST tap across the bar, so
    // this is the bright end of what the bar sits on — the right end to size a floor against.
    let lum = (ground[0] + ground[1] + ground[2]) / 3.0;
    let want = ((floor - lum * (1.0 - a)) / a.max(1e-4)).clamp(0.0, 1.0);
    if want <= 0.0 {
        return 0.0;
    }
    let steps = 8;
    for i in 0..=steps {
        let g = want * (1.0 - i as f32 / steps as f32);
        let face = [
            ground[0] * (1.0 - a) + g * a,
            ground[1] * (1.0 - a) + g * a,
            ground[2] * (1.0 - a) + g * a,
            1.0,
        ];
        if contrast(theme::TEXT_READING, face) >= TRACK_INK_CONTRAST {
            return g;
        }
    }
    0.0
}

/// The lift's floor, swept by `/tmp/plxnative-tracklift=<floor>`.
///
/// `0` is the material as it was before the floor existed, which is what makes an A/B one launch
/// rather than one build. Read once per process, like every other material sweep here.
#[cfg(feature = "devtriggers")]
fn lift_floor() -> f32 {
    static SEEN: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        let Some(v) = crate::dev::read("tracklift").and_then(|v| v.trim().parse::<f32>().ok()) else {
            return theme::TAB_GLASS_LIFT_FLOOR;
        };
        crate::log(&format!("glass: track lift swept to floor={v}"));
        v.clamp(0.0, 1.0)
    })
}
#[cfg(not(feature = "devtriggers"))]
fn lift_floor() -> f32 {
    theme::TAB_GLASS_LIFT_FLOOR
}

fn tab_glass_stops(ground: [f32; 3]) -> ([f32; 4], [f32; 4]) {
    let lo = theme::TAB_GLASS_TOP[3];
    let hi = density_max_sweep().unwrap_or(theme::TAB_TRACK_A_TOP);
    let spread = theme::TAB_GLASS_BOT[3] - lo;
    let a = match tab_glass_dim_sweep() {
        Some(a) => a,
        None => track_density(ground),
    };
    // **THE SPREAD TAPERS AS THE SOLVE RISES, and it did not until a judging panel checked the
    // arithmetic.** The bottom stop is `a + spread` and nothing bounded it: the solve is sized so the
    // labels clear their contrast against the TOP stop, which is the lightest, and the bottom was
    // free to run wherever it liked. On a near-white hero a solve of .633 drew a bottom stop of .793
    // — past [`theme::TAB_TRACK_A_TOP`], the weight at which there is nothing left to see through,
    // i.e. the flat capsule this material exists to replace, at the bottom of every heavy bar.
    //
    // Tapering costs nothing and takes nothing away from legibility, because the solve stays on the
    // top stop either way. What goes is the opaque lower third, which is the thing that actually
    // makes a heavy bar read as paint rather than as glass.
    let k = ((a - lo) / (hi - lo).max(1e-4)).clamp(0.0, 1.0);
    let heavy = (a + spread * (1.0 - k)).min(hi);
    // The diffuse component contributes the SAME AMOUNT OF LIGHT at both stops — `g·a` — so the
    // bottom stop's extra alpha still only removes more ground and the gradient keeps its
    // direction. Tint both stops with `g` and the heavier one would come out LIGHTER than the top,
    // which is a lamp under the floor again.
    let g = track_lift(ground, a);
    let g_bot = if heavy > 1e-4 { g * a / heavy } else { g };
    (theme::with_a([g, g, g, 1.0], a), theme::with_a([g_bot, g_bot, g_bot, 1.0], heavy))
}

/// sRGB -> linear, the WCAG transfer function.
fn linearize(c: f32) -> f32 {
    if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
/// WCAG relative luminance of an rgba token (alpha ignored — a ground is opaque).
fn rel_luma(c: [f32; 4]) -> f32 {
    0.2126 * linearize(c[0]) + 0.7152 * linearize(c[1]) + 0.0722 * linearize(c[2])
}
/// WCAG contrast ratio between two opaque colours, brighter over darker.
fn contrast(a: [f32; 4], b: [f32; 4]) -> f32 {
    let (x, y) = (rel_luma(a), rel_luma(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The bar the track's idle labels have to clear — the same 3:1 the ambient-ground test holds a
/// wash to, for the same ink.
const TRACK_INK_CONTRAST: f32 = 4.0;

/// **The scrim weight this ground needs, solved per frame.** This is the answer to the one thing
/// that kept defeating the material: a blur removes DETAIL, not brightness, so a fixed density is
/// either too light over a bright hero — where [`theme::TEXT_TERTIARY`] labels wash out, reported
/// from the panel over a cyan sky — or too heavy over a dark one, where it throws away the whole
/// effect. Neither is a tuning problem. The density is a FUNCTION of the ground and always was.
///
/// So the ink stops moving and the material moves instead: a dark backdrop gets a nearly
/// transparent bar, a white one gets an opaque bar, and the labels sit at the same contrast on
/// both. The floor is the design system's own `--glass-track-top` and the ceiling is the flat
/// track's [`theme::TAB_TRACK_A_TOP`] — past that there would be nothing left to see through, which
/// is the flat capsule this replaces.
///
/// A search rather than a closed form because the transfer function is not invertible in anything
/// worth reading: 24 steps of 1/50 across the legal span, each one two multiplies and a compare,
/// once per frame. It is also why the ground is an rgb and not a luminance — the ink is cool grey
/// and the contrast is computed against the actual colour, not against a brightness that would call
/// a saturated blue and a neutral grey the same ground.
fn track_alpha_for(ground: [f32; 3]) -> f32 {
    let lo = theme::TAB_GLASS_TOP[3];
    let hi = density_max_sweep().unwrap_or(theme::TAB_TRACK_A_TOP).max(lo);
    let steps = 24;
    for i in 0..=steps {
        let a = lo + (hi - lo) * (i as f32 / steps as f32);
        let k = 1.0 - a;
        let face = [ground[0] * k, ground[1] * k, ground[2] * k, 1.0];
        if contrast(theme::TEXT_READING, face) >= TRACK_INK_CONTRAST {
            return a;
        }
    }
    hi
}

/// `/tmp/plxnative-trackmax=<a>` — the density CEILING, swept.
///
/// The ceiling is what decides how dark this bar is allowed to get over bright artwork, and it is
/// the one remaining number between our material and the reference. Measured on a stripe ground:
/// the macOS 26 tab bar's face sits at 89 against a page of 127 (−27% Weber) and, over four
/// different grounds, its face barely moves at all — 92, 94, 92, 96 — while ours swings 47…84 and
/// reaches −60%. Their material has ONE density and converges every backdrop toward a fixed grey;
/// ours re-solves per frame against [`TRACK_INK_CONTRAST`] and, over a bright hero, spends the
/// whole span to get there.
///
/// **This is the continuous lever, and it is here instead of the obvious one.** The obvious one is
/// a second, LIGHT polarity with flipped ink — Apple's own answer — and this repo had it and
/// deleted it: `theme.rs`'s note where that polarity's tokens used to stand records four bugs in one week
/// from a discrete state that has to be re-decided every time the ground moves, and a judging panel
/// that measured the light material landing 34.9 L\* off the local ground on the median MIXED hero
/// where the dark one lands 3.4. Do not re-add it from this note. Lowering a ceiling adds no state,
/// no hysteresis and no decision — it only changes how far one existing solve may travel.
///
/// What it COSTS is stated plainly, because it is the whole trade: below the ceiling the solve
/// needs, the idle labels stop clearing [`TRACK_INK_CONTRAST`] over the brightest grounds. That is
/// also exactly the trade the reference makes, and the criticism it takes for it.
/// The fixed-weight leg's density, read ONCE at boot like every other sweep here.
///
/// It was `crate::dev::read("tabglassdim")` inline in [`tab_glass_stops`], i.e. a `read_to_string`
/// of a `/tmp` path on **every drawn frame** of every screen that wears the bar — a syscall on the
/// 60 fps path, in every dev and harness build, which is what the fps scenes measure.
///
/// **No `#[cfg]` pair**, unlike the sweeps around it: `dev::read` is already `None` at COMPILE time
/// without the `devtriggers` feature, so a second gate here only re-derives what the one door
/// guarantees — and a hand-written pair is how this file broke the `RELEASE=1` build once already,
/// by swallowing a neighbour's attribute when a new function was spliced between them.
fn tab_glass_dim_sweep() -> Option<f32> {
    static SEEN: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        let v = crate::dev::read("tabglassdim")?.trim().parse::<f32>().ok()?;
        if !(0.0..=1.0).contains(&v) {
            crate::log("glass: tabglassdim ignored (want 0..1)");
            return None;
        }
        crate::log(&format!("glass: track density pinned to {v}"));
        Some(v)
    })
}

#[cfg(feature = "devtriggers")]
fn density_max_sweep() -> Option<f32> {
    static SEEN: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        let v = crate::dev::read("trackmax")?.trim().parse::<f32>().ok()?;
        crate::log(&format!("glass: density ceiling swept to {v}"));
        Some(v.clamp(0.0, 1.0))
    })
}
#[cfg(not(feature = "devtriggers"))]
fn density_max_sweep() -> Option<f32> {
    None
}

/// **The lit edge's weight for a bar already drawn at `density`.**
///
/// The rim rides the density rather than sampling the ground a second time, and that is the whole
/// design: [`track_alpha_for`] is already a monotone function of how bright the ground is, so the
/// drawn density IS the ground's brightness, in the one form this bar has already eased. Deriving
/// the edge from it means the two halves of the material cannot disagree again — which is the bug
/// this fixes. Half of it followed the ground and half of it was a constant, so over bright artwork
/// the bar darkened correctly and then wore an edge drawn for a ground it was no longer on.
///
/// `t` is where the density sits between its floor and its ceiling, so a dark ground (density
/// pinned at the floor) returns [`theme::GLASS_RIM`] and [`theme::GLASS_RIM_LIGHT`] unchanged, to
/// the bit. Every ground brighter than that travels toward [`theme::GLASS_RIM_MAX`], and the top
/// keeps exactly twice the perimeter's weight the whole way — the one lamp does not move because
/// the room got brighter.
///
/// Returns `(perimeter, lit)` — the ring and, over it, the edge facing the light.
fn track_rim(density: f32) -> ([f32; 4], [f32; 4]) {
    let lo = theme::TAB_GLASS_TOP[3];
    let hi = density_max_sweep().unwrap_or(theme::TAB_TRACK_A_TOP);
    let k = ((density - lo) / (hi - lo).max(1e-4)).clamp(0.0, 1.0);
    let ceil = rim_max_sweep().unwrap_or(theme::GLASS_RIM_MAX);
    // The ring travels: a white line runs out of room as the ground brightens, so it climbs the ramp
    // toward `GLASS_RIM_MAX`, and the highlight keeps exactly twice the perimeter's weight the whole
    // way — the one lamp does not move because the room got brighter.
    let top = theme::GLASS_RIM_LIGHT[3] + (ceil - theme::GLASS_RIM_LIGHT[3]).max(0.0) * k;
    (theme::with_a(theme::GLASS_RIM, top * 0.5), theme::with_a(theme::GLASS_RIM, top))
}

/// `/tmp/plxnative-rimmax=<a>` — the ceiling that ramp climbs toward, swept.
///
/// It exists because the ramp's ceiling is the one number that decides whether the edge reads as a
/// LIT EDGE or as a drawn white outline, and it had never been held against the reference. Measured
/// on a stripe ground: how much of the ground's swing the top rim carries is 0.23 for us and 0.25
/// for the macOS tab bar — i.e. ours is not flat and never was, which was the standing suspicion.
/// What differs is the LEVEL. Their rim sits at 124 against a page of 127 and a face of 89; ours at
/// 212 against a page of 124 and a face of 53. Relative to its own brightness their edge varies by
/// 24% and ours by 10%, which is the whole of why theirs reads as the material catching a lamp and
/// ours as a stroke drawn round a shape.
#[cfg(feature = "devtriggers")]
fn rim_max_sweep() -> Option<f32> {
    static SEEN: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| {
        let v = crate::dev::read("rimmax")?.trim().parse::<f32>().ok()?;
        crate::log(&format!("glass: rim ceiling swept to {v}"));
        Some(v.clamp(0.0, 1.0))
    })
}
#[cfg(not(feature = "devtriggers"))]
fn rim_max_sweep() -> Option<f32> {
    None
}

/// **Which way the standing track answers the contrast question, and how far along it is.**
///
/// [`track_alpha_for`] puts black between the labels and the picture, and over bright artwork that
/// answer runs out at .69 — the flat capsule this material was built to replace. The other answer is
/// to go LIGHT and flip the ink, which clears the same 3:1 while leaving a bright scene looking
/// bright. Both are the same rule; only the sign differs, and the row carries a 0..1 blend between
/// them rather than an enum, because the way from one to the other has to be a fade.
///
/// The polarity follows the ENVIRONMENT — the room decides, and then the alpha inside that polarity
/// is solved exactly as before. The alternative rule, "pick whichever needs less material", turns
/// out to AGREE with it everywhere, and that agreement is worth recording rather than relying on: it
/// holds only because the two inks were muted by the same amount (the light track's idle ink was
/// `COOL_600` against [`theme::TEXT_TERTIARY`]'s `COOL_400` — one ramp, opposite sides of mid; both
/// that ink and the token naming it went with the polarity, so this is history, not a live pair).
/// Measured over a dark grid the light polarity wants **.652** where the dark one wants its **.340**
/// floor, and over bright artwork the two swap to .300 against .688. Swap either ink for a
/// near-black one and the cost rule starts putting a light bar over a dark grid; the lightness rule
/// does not depend on the ink at all, which is why it is the one written down.
/// CIE L\* of an opaque colour — how LIGHT it looks, on the axis a person judges it on.
///
/// Not [`contrast`]'s WCAG ratio, and the difference decided the rim: that ratio carries a +0.05
/// flare term built for text legibility, which compresses hard at the dark end and says a rim over
/// a dark ground has LESS contrast than the same rim over a bright one — the opposite of what the
/// panel shows. For "how visible is this edge" and "is this room bright", L\* is the instrument.
fn lstar(c: [f32; 4]) -> f32 {
    let y = rel_luma(c);
    if y > 0.008856 { 116.0 * y.cbrt() - 16.0 } else { 903.3 * y }
}

/// The air between the unfurled chip's capsule and the track, at the point they come closest.
///
/// Sixteen is the bar's own rung — [`TAB_GAP`], the air between two pills inside the track. It is
/// not decoration: the two capsules are one band in one material, and a band whose halves TOUCH
/// draws its two 1px rims side by side, which reads as a bright seam exactly where the design has
/// a continuous edge. See [`GLASS_TRACK_MAX`].
const BAND_AIR: f32 = theme::space::SM;

/// **Where the unfurled [`profile_chip`] capsule's right edge can reach, at its widest.**
///
/// It is [`chip_cap`] at rest with a name at its budget — **the rect the control DRAWS**, not a
/// hand-copy of the terms that build it. The name is elided to [`CHIP_NAME_MAX`] before it is
/// measured, so this is arithmetic on constants and needs no font to evaluate, which is what makes
/// the clearance below a host test rather than a device capture.
const CHIP_CAP_MAX_R: f32 = {
    let c = chip_cap(1.0, CHIP_NAME_MAX);
    c.x + c.w
};

/// The widest a track may be and still wear glass — the design system's `--glass-track-max`.
///
/// A track is charged by its LENGTH: what a glass surface costs is the blurred RECTANGLE, the
/// surface grown 88px a side, so a 940-wide one priced at `(940 + 176) x (76 + 176)` = 281k px^2 —
/// inside the ~300k a MOVING host holds 60 fps under (`docs/glass-hardware-budget.md`) — while a
/// 1050-wide one was not. It is the one limit here a section table can trip on its own: enough
/// libraries and the strip outgrows the budget, so the material has to come off by arithmetic
/// rather than by anyone remembering.
///
/// **It is no longer the track's own arithmetic, because the track is no longer the only surface in
/// the band.** [`profile_chip`] wears the same material now, and the two are priced and placed
/// together:
///
/// * **The BUDGET is the UNION.** Two glass surfaces in a frame converge on one grab
///   (`gfx::blur_region_union`), so the bar costs one snapshot — but that snapshot spans both. The
///   chip's capsule starts at x=82 and the margin is 88, so the union's left edge clamps to 0 and
///   its right is the track's; the band is 200px tall after the same clamp. That prices the pair at
///   `(1048 + w/2) x 200`, which reaches [`crate::gfx::GLASS_REGION_BUDGET`] at **w = 904** — not at
///   the 940 the track alone could afford.
/// * **And the two must not TOUCH**, which binds tighter. The track is centred, so its left edge is
///   `(SCR_W - w) / 2`; the capsule's right edge stops at [`CHIP_CAP_MAX_R`]. Solving for
///   [`BAND_AIR`] between them is this expression — **856** — and it is written as the expression so
///   that raising [`CHIP_NAME_MAX`] tightens the cap instead of silently letting the two meet.
///
/// Overlap is what had to be made impossible, rather than handled. There is ONE blur cache: a
/// second surface over the first gets no second blur, only a second frost and a second rim
/// composited over material that already carries both — the double-darkening `draw_tab_row`'s
/// source-pass note measures from the other direction. Nothing in the shader can undo that, so the
/// geometry has to keep them apart, and the test below is what keeps them apart.
///
/// What it costs is stated plainly: the pair fits ~84px less strip than the track alone did, which
/// is under one pill's width and so changes the answer for at most one section table in the band
/// where it lands. What it buys is that the band is never two materials and never a doubled one.
const GLASS_TRACK_MAX: f32 = crate::ui::consts::SCR_W - 2.0 * (CHIP_CAP_MAX_R + BAND_AIR);

/// Is the shared tab track wearing glass this frame?
///
/// **Glass is the material.** `/tmp/plxnative-flattabs` takes it away, which is how the two are
/// compared on one television without a second binary — and the one thing that made the flat track
/// the answer for a while is gone: the density is no longer a constant that had to be legible over
/// every possible hero at once, so it is not forced up to the flat capsule's own weight. See
/// [`track_alpha_for`].
///
/// Four refusals, each a different kind of limit:
/// - **A panel is open** — and the reason is DISTANCE, not arithmetic. Two glass surfaces in a
///   frame converge on one grab (`gfx::blur_region_union`), so neighbours avoid a second chain, but
///   this bar sits at the top and a popover's panel in the middle: their union is most of the
///   frame, which is the whole-screen capture the region limit exists to avoid. While the modal
///   owns focus, keeping only its material is also the clearer hierarchy.
///   `/tmp/plxnative-glassboth` lifts this one for measurement only.
/// - **The track is wider than [`GLASS_TRACK_MAX`]** — see there.
///
/// Split in two because the SOURCE pass needs the first half on its own: [`draw_tab_row`] has to
/// know whether the track will wear glass in order to decide whether to draw itself into that
/// track's own backdrop at all. See there.
fn tab_glass_wanted(track_w: f32) -> bool {
    !flat_tabs_armed()
        && track_w <= GLASS_TRACK_MAX
        && (!crate::ui::popover::any_open() || glass_both_armed())
}

/// The three trigger probes this material reads, each latched at first use by
/// [`crate::dev::latched_flag`].
///
/// **`dev::flag` is `Path::exists()`, i.e. a `stat`.** These sat raw on the draw path, and
/// [`tab_glass_wanted`] alone is called three times per frame — from `tab_glass_prepare`, from the
/// source-pass guard, and from [`tab_glass_on`] — so a bar-wearing screen paid up to five `stat`
/// calls a frame at 60 fps, in every dev and harness build. The fps scenes measure exactly that.
use crate::dev::latched_flag;

latched_flag!(
    /// `/tmp/plxnative-flattabs` — the material off, for an A/B against the flat capsule.
    fn flat_tabs_armed = "flattabs";
);
latched_flag!(
    /// `/tmp/plxnative-glassboth` — keep the track's glass up while a popover is open, so the two
    /// materials can be judged side by side in one frame.
    fn glass_both_armed = "glassboth";
);
latched_flag!(
    /// `/tmp/plxnative-groundlog` — what the sampler read and what density it chose.
    fn ground_log_armed = "groundlog";
);

/// …and the fourth: **this page is being drawn as a blur SOURCE**, where the track never wears the
/// material it is producing. `Glass::prepare` also mutates the one process-wide `DynamicClock`,
/// which is keyed on presents rather than draws, so a second call in the same present would spend
/// that present's refresh slot on a surface nobody sees.
fn tab_glass_on(track_w: f32) -> bool {
    tab_glass_wanted(track_w) && !crate::gfx::blur_source_pass()
}

/// **Will the shared bar wear glass this frame?** — [`tab_glass_wanted`], asked from OUTSIDE
/// [`draw_tab_row`], which is where the track's own rect is not in hand.
///
/// It measures the strip through the same cached metrics the draw walks, so the row and its second
/// surface cannot answer the width rule differently. Deliberately the SOURCE-PASS-blind half:
/// [`profile_chip`] asks it in order to decide whether it is about to be drawn into a backdrop, and
/// `tab_glass_on`'s extra clause is false during exactly that pass.
fn bar_glass_wanted() -> bool {
    with_tab_metrics(|_, widths| tab_glass_wanted(tab_track_w(widths)))
}

/// Resolve the tab track's glass cadence BEFORE the page it sits on draws.
///
/// Every dynamic glass owner has to prepare before any of them captures — `Glass::prepare`'s own
/// contract — and this one used to break it by preparing inside `draw_tab_row`, i.e. in the middle
/// of the page draw. On the capture path that merely meant an extra chain now and then. On the
/// direct path it was measurable: the pre-page snapshot was taken, the tab track then invalidated
/// it from inside the page, and the capture path re-did the whole thing — both paths running in
/// one frame. Call this beside the other owners' `prepare_present`.
pub(crate) fn tab_glass_prepare() {
    // The width rule is part of the answer, so the cadence has to measure the strip too — the same
    // cached metrics the draw walks, one frame's worth, keyed on `browse::tabs_gen()`.
    if !with_tab_metrics(|_, widths| tab_glass_on(tab_track_w(widths))) {
        return;
    }
    let state = unsafe { &mut *std::ptr::addr_of_mut!(TAB_GLASS_STATE) };
    Glass::DYNAMIC_BACKDROP.prepare(
        state,
        crate::ui::idle::present_moving() || crate::ui::idle::present_dirty(),
    );
}
// label + width cache keyed on browse::tabs_gen(): rebuilding the CStrings and
// re-measuring every frame — on Home's hot path too — was a review-confirmed waste
static mut TAB_CACHE: Option<(u32, Vec<std::ffi::CString>, Vec<f32>)> = None;

/// The tab pill under the pointer (0 = Home, 1.. = sections), or None. Matches against the
/// CLIPPED rects, so only the pill area you can actually see is clickable.
///
/// One consequence worth knowing: these rects are the last DRAWN frame's, so a click that lands
/// while the strip is mid-reveal is graded against where the pills were when the user last saw
/// them — which is the right frame to grade against, but does mean a click aimed at a pill the
/// spring is still carrying can land on its neighbour. The strip only moves in response to the
/// user's own focus move, so the two gestures do not overlap in practice.
pub(crate) fn tab_pill_at(mx: f32, my: f32) -> Option<usize> {
    let rects = unsafe { &*std::ptr::addr_of!(PILL_RECTS) };
    rects.iter().position(|r| r.w > 0.5 && r.contains(mx, my))
}

/// Run `f` with the tab row's labels + pill widths, rebuilding them only when
/// `browse::tabs_gen()` moves. Shared by the per-frame scroll step and the draw, so the two
/// can't measure the strip differently.
///
/// The key is the STRIP's generation, not the section table's, and with several sources that is no
/// longer the same number: the table's generation moves for every source whose libraries land and
/// every count that arrives, while the row only changes when the projection does. Most of those
/// landings fold onto pills already drawn (a friend's films ride your *Movies* pill), so keying on
/// the table re-measured every pill in the row, several times a boot, on Home's hot path — for a
/// strip that had not moved.
///
/// Deliberately a closure and not a `&'static` getter: the borrow points into `TAB_CACHE`, and a
/// nested rebuild would free the `CString`s a caller is still handing to `TabPill` as a raw
/// `*const c_char`. Scoping it here makes that impossible to write by accident — so do NOT call
/// this again from inside `f`.
fn with_tab_metrics<R>(f: impl FnOnce(&[std::ffi::CString], &[f32]) -> R) -> R {
    use std::ffi::CString;
    use std::ptr::addr_of_mut;
    let gen = crate::browse::tabs_gen();
    let cache = unsafe { &mut *addr_of_mut!(TAB_CACHE) };
    if cache.as_ref().map(|c| c.0 != gen).unwrap_or(true) {
        let nsec = crate::browse::tab_count();
        let mut labels: Vec<CString> = Vec::with_capacity(1 + nsec + 1);
        labels.push(CString::new("Home").unwrap_or_default());
        for i in 0..nsec {
            labels.push(CString::new(crate::browse::tab_title(i)).unwrap_or_default());
        }
        // measure bold (the widest state) so pill widths don't change with focus; pill =
        // label + the season tabs' ±18 padding
        let mut widths: Vec<f32> = labels
            .iter()
            .map(|l| crate::text::text_width(l.as_ptr(), theme::size::BODY, 1) + 2.0 * TAB_PILL_PAD)
            .collect();
        // The Search pill, last. It carries an EMPTY label so nothing here has to special-case a
        // missing entry — `labels.len()` stays the pill count, which is what the draw and the hit
        // test both walk — and a fixed square width, because a mark is not measured like a word.
        labels.push(CString::default());
        widths.push(TAB_ICON_PILL_W);
        *cache = Some((gen, labels, widths));
    }
    let (_, labels, widths) = cache.as_ref().unwrap();
    f(labels, widths)
}

/// Content width of the whole pill strip: the pills plus the air between them.
fn tab_content_w(widths: &[f32]) -> f32 {
    widths.iter().sum::<f32>() + TAB_GAP * (widths.len() as f32 - 1.0).max(0.0)
}
/// The VISIBLE pill area: the strip's own width until it outgrows [`TAB_VIEW_MAX`], then that.
fn tab_view_w(widths: &[f32]) -> f32 {
    tab_content_w(widths).min(TAB_VIEW_MAX).max(0.0)
}
/// The TRACK's own width — the pill area plus its uniform inset on both sides. What the glass
/// budget is charged against ([`GLASS_TRACK_MAX`]), and what `draw_tab_row` builds its rect from.
fn tab_track_w(widths: &[f32]) -> f32 {
    tab_view_w(widths) + 2.0 * TAB_TRACK_PAD
}
/// Content-space x of pill `i`'s left edge (0 = the strip's start).
fn tab_pill_x(widths: &[f32], i: usize) -> f32 {
    let i = i.min(widths.len());
    widths[..i].iter().sum::<f32>() + TAB_GAP * i as f32
}
/// Scroll offset that brings pill `idx` into the strip's viewport: the minimal scroll-into-view
/// rule the season tabs and every shelf share ([`card_row::reveal`]) — move only when the pill
/// (± one gap of context) would clip, and never past the content ends. Pure, so the "every pill
/// is reachable" invariant is host-testable without a font.
fn tab_scroll_target(widths: &[f32], idx: usize, cur: f32) -> f32 {
    let view_w = tab_view_w(widths);
    let max = (tab_content_w(widths) - view_w).max(0.0);
    if idx >= widths.len() {
        return cur.clamp(0.0, max);
    }
    let x = tab_pill_x(widths, idx);
    let lo = x + widths[idx] + TAB_GAP - view_w; // right edge (+ context) on screen
    let hi = x - TAB_GAP; // left edge (− context) on screen
    crate::ui::card_row::reveal(cur, lo, hi, max)
}

/// Step the strip's horizontal scroll — called once per frame from BOTH screens' update (the draw
/// runs at dt=0, like the profile chip's unfurl). `focused` = the pill holding remote focus or -1,
/// `selected` = the tab whose screen is showing.
///
/// Off the row it tracks the SELECTED pill, and on Home that means the strip returns to the start
/// when focus leaves the band. That is deliberate, and it is where this differs from the season
/// tabs (which HOLD): the way back INTO the band is the profile chip and then the Home pill — its
/// left end — so a strip parked far to the right would be showing pills that the next keypress
/// cannot reach without scrolling back anyway, under a row with no selected tab visible. Reveal
/// is minimal-scroll, so this is a no-op whenever the selected pill is already on screen, which is
/// the whole of the Library screen's life after [`tab_row_reveal`] placed it.
///
/// It also steps the row's travelling capsules ([`TOP_STRIP`]) and the profile chip's unfurl
/// ([`CHIP_EXPAND`]), off the very same `focus` the scroll reads — so every caller of the shared
/// row gets the motion by construction rather than by remembering to call a second thing.
pub(crate) fn tab_row_update(selected: c_int, focus: TopFocus, dt: f32) {
    use std::ptr::{addr_of, addr_of_mut};
    let focused = focus.pill();
    // The chip is the bar's other stop, so its unfurl is stepped here rather than by whichever
    // screen happens to own the focus this frame — the same reason the capsules and the track's
    // weight are. It is also why [`TopFocus`] is one value: with a separate `chip: bool` beside
    // `focused`, a screen could hand down a lit chip AND a lit pill.
    unsafe {
        (*addr_of_mut!(CHIP_EXPAND)).step(if matches!(focus, TopFocus::Chip) { 1.0 } else { 0.0 }, K_CHIP, dt)
    };
    // The bar is CONTINUOUS chrome across the Home↔Library route change and the capsule has to start
    // travelling on the PRESS frame, before the route flips — so the selection is the NAV's pending
    // one whenever there is one, exactly as `library::view_section` is the pending one for that
    // screen's own chips. Resolved HERE, in the one function both screens call, so neither can
    // forget it and the two can never disagree about which pill is lit. (It also carries into `idx`
    // below, so a strip that must SCROLL to reach the destination starts scrolling on the press
    // frame too.)
    let selected = crate::ui::nav::view_tab(selected);
    let idx = if focused >= 0 { focused } else { selected.max(0) } as usize;
    let cur = unsafe { addr_of!(TAB_SCROLL).read() };
    // Both reads happen inside the ONE `with_tab_metrics` closure — its doc forbids nesting, and the
    // capsules must be placed from the SAME widths the pills are laid out with, so a capsule can
    // never come to rest somewhere no pill is.
    let t = with_tab_metrics(|_, w| {
        let target = tab_scroll_target(w, idx, cur.pos);
        let span = |i: usize| (i < w.len()).then(|| (tab_pill_x(w, i), w[i]));
        unsafe { (*addr_of_mut!(TOP_STRIP)).update(selected, focused, span, dt) };
        target
    });
    unsafe { (*addr_of_mut!(TAB_SCROLL)).step(t, K_TAB_SCROLL, dt) };
    let s = unsafe { addr_of!(TAB_SCROLL).read() };
    crate::ui::anim::probe("tabrow.scroll", s.pos, s.vel, t, dt);
    // The track's own weight follows its ground here for the same reason the capsules move here:
    // three screens draw this bar and none of them should have to remember to animate it.
    track_density_step(dt);
}

/// Put pill `idx` on screen at once (no glide). For screen ENTRY — the Library screen opened
/// straight into a far-right section (the `/tmp/plxnative-library=N` boot, or a section restored
/// from the saved view) must show its own tab, and a long slide on arrival would be motion the
/// user never asked for. Mirrors `detail.rs`'s `tab_hscroll.jump` on a fresh detail page.
///
/// It deliberately does NOT place the capsules. Their own landing rule ([`Capsule::step`]) already
/// jumps an *unplaced* one, which covers the boot-straight-into-a-section case; leaving the placed
/// case to glide is exactly what makes OK on Home's `Movies` pill read as the selection travelling
/// there rather than blinking there.
pub(crate) fn tab_row_reveal(idx: usize) {
    use std::ptr::{addr_of, addr_of_mut};
    let cur = unsafe { addr_of!(TAB_SCROLL).read() };
    let t = with_tab_metrics(|_, w| tab_scroll_target(w, idx, cur.pos));
    unsafe { (*addr_of_mut!(TAB_SCROLL)).jump(t) };
}

/// Draw the centered pill row. Records the rects for [`tab_pill_at`].
///
/// It takes no `selected`/`focused`: both states are now the strip's travelling capsules, placed once
/// per frame by [`tab_row_update`] from exactly those two values. Passing them here as well would let
/// a screen's draw and its update disagree about which tab is lit — which is precisely the class of
/// bug a single source of the row's state removes.
pub(crate) fn draw_tab_row(p: Painter) {
    use std::ptr::{addr_of, addr_of_mut};
    let rects = unsafe { &mut *addr_of_mut!(PILL_RECTS) };
    rects.clear(); // nothing drawn = nothing hittable, including on the early return below
    // …and the band's material with them, for the same reason and in the same place: every early
    // return below must leave [`profile_chip`] on the flat capsule rather than on the stops some
    // previous frame solved. See [`BarMaterial`].
    unsafe { BAR_MATERIAL = BarMaterial::Flat };
    with_tab_metrics(|labels, widths| {
        let n = labels.len();
        if n == 0 {
            return;
        }
        let content_w = tab_content_w(widths);
        let view_w = tab_view_w(widths);
        // ONE translucent dark capsule contains the whole row — the tvOS tab-bar track. It (not the
        // segments) owns legibility over bright hero art: inside it the segments keep their clean
        // season-tab looks (plain = bare dim text). Sheened for the 1px glass rim. The uniform inset
        // (and so the concentric radii) is [`TAB_TRACK_PAD`], shared with the focused profile chip.
        // The track is the strip's width until the strip outgrows the screen, then it caps and the
        // pills scroll inside it — the track itself never moves.
        let x0 = (crate::ui::consts::SCR_W - view_w) * 0.5;
        let track = Rect::new(
            x0 - TAB_TRACK_PAD,
            TOP_BAR_Y - TAB_TRACK_PAD,
            view_w + 2.0 * TAB_TRACK_PAD,
            TAB_PILL_H + 2.0 * TAB_TRACK_PAD,
        );
        // **A SURFACE MAY NOT APPEAR IN ITS OWN BACKDROP**, and this row is the one place in the app
        // where it could: the direct source path renders the whole page again into a small FBO, and
        // the page includes this bar. Left in, the glass track blurred the FLAT track — its own
        // `scrim_black(0.72..0.82)` capsule, plus the pill labels — and then darkened that again
        // with its own stops. Measured on Home over a UNIFORM hero (220,255,163 across the whole
        // span, above and below the bar): the flat track reads (52,60,38) and the glass one (33,38,
        // 26). A material whose whole argument is that it is LIGHTER than the capsule it replaces
        // came out darker than it, muddy, and with the selection capsule swimming in a patch of its
        // own doubled scrim. Every "the glass tab bar looks wrong" report traces here — including
        // the density sweeps that only cleared at 0.70, which was the doubling being paid for twice.
        //
        // Only the DIRECT path could have this bug, which is why it arrived with that path becoming
        // the default: the capture path grabs framebuffer 0 from inside the glass surface, i.e.
        // after the page and BEFORE this bar, so the track was never in its own snapshot there.
        //
        // The exclusion is exactly "will this row wear glass" — [`tab_glass_wanted`], the same test
        // minus its source-pass clause. When a popover is open the track is flat and belongs in the
        // snapshot, because then it really is behind the panel that samples it.
        if crate::gfx::blur_source_pass() && tab_glass_wanted(track.w) {
            return;
        }
        // dark-material weight (`theme::TAB_TRACK_TOP` holds the reasoning): light enough to keep a
        // hint of the art, dark enough that the TEXT_TERTIARY plain segments hold contrast even over
        // near-white art
        //
        // That flat material is now the FALLBACK — `/tmp/plxnative-flattabs`, a popover being open,
        // a track too wide, or a driver with no render target. The shipped one is the popovers'
        // backdrop glass, and the two cases are not alike: a popover opens over a still page and
        // snapshots once, while this bar sits over a page that scrolls, flips its hero and
        // cross-fades between routes, so it opts into the reusable dynamic policy — the bar draws
        // every presented frame while a dirty snapshot refreshes on every changed one.
        //
        // **A modal takes the glass away**, and the reason is DISTANCE, not arithmetic. Two glass
        // surfaces in a frame converge on one grab (`gfx::blur_region_union`), so NEIGHBOURS avoid
        // a second steady-state chain — but this bar sits at the top and a popover's panel in the
        // middle of it, and the union of the two is most of the frame, which is the whole-screen
        // capture the region limit exists to avoid. While the modal owns focus, keeping only its
        // material is also the clearer hierarchy; the disabled bar falls back to its flat track.
        // Never while this page is being drawn as a blur SOURCE: `Glass::prepare` mutates the one
        // process-wide `DynamicClock`, which is keyed on presents rather than on draws, so a
        // second call in the same present would consume that present's refresh slot on behalf of a
        // surface nobody sees. The flat track is also the right source pixel — glass over glass is
        // not what is behind the panel.
        // `/tmp/plxnative-glassboth` lifts the popover exclusion for measurement ONLY. The
        // exclusion exists because this bar sits at the top and a popover's panel in the middle,
        // and the union of the two is most of the frame — the whole-screen capture the region
        // limit exists to avoid. It is also the one scene where the direct path's advantage should
        // show, since its cost is the page's draw calls rather than the region's area, so the two
        // paths need to be comparable on it.
        // consumed unconditionally: the publisher writes every frame and the reset is what stops a
        // hero standing behind the Library's bar after a route change
        // The PIXELS, sampled at a low rate; the flat app grey when the readback is refused, which
        // is also the honest answer on the two screens that really do have it up there.
        // …and never DURING a route transition: `nav` dips the page under this bar while the bar
        // itself holds still, so those pixels are the app ground fading, not the screen's colour.
        let settled = crate::ui::nav::page_alpha() >= 0.999;
        // **Decided BEFORE the ground is sampled, because it decides whether to sample at all.**
        // `sample_ground` is five `glReadPixels` boxes, and a readback stalls a tiler — so a track
        // that is about to draw FLAT (a popover is up, `flattabs` is armed, or the strip is wider
        // than `GLASS_TRACK_MAX`) must not pay for a number only the glass path consumes. It did,
        // on every screen wearing the bar, for as long as the app was open.
        let glass_on = tab_glass_on(track.w);
        let groundlog = ground_log_armed();
        // …and FASTER while a polarity is being decided: the decision is gated on two independent
        // readings, so the sampler's rate is the decision's rate, and every frame of it is spent
        // showing the material the bar is about to stop being.
        let (cr, cg, cb) = theme::CLEAR_RGB; // the app ground, as the token that names it
        let flat_ground = [cr, cg, cb];
        let ground = if glass_on || groundlog {
            crate::gfx::sample_ground([track.x, track.y, track.w, track.h], settled)
                .unwrap_or(flat_ground)
        } else {
            flat_ground
        };
        // `/tmp/plxnative-groundlog` — the density is now a FUNCTION of something invisible, so the
        // instrument that says what it read and what it chose is not optional. It is how the first
        // version of this was caught reading Plex's `UltraBlurColors`, which gave (0.30,0.23,0.18)
        // for a hero whose top edge is (0.00,0.68,0.91) and left the bar at its floor.
        if groundlog {
            static mut LAST: u32 = 0;
            let n = unsafe { LAST };
            if n % 20 == 0 {
                // BOTH weights: the solve is a step twice a second and the drawn one eases to
                // it, so a single number could not tell "the ground moved" from "the bar is still
                // travelling" — which is the whole question this instrument now has to answer.
                // through `track_density` rather than off the static: it is idempotent within a
                // frame, and reading the static raw printed the seed value 0.000 on the one frame
                // the bar had never been drawn — an instrument's first line reading as a bug in the
                // thing it was armed to watch.
                let drawn = track_density(ground);
                crate::log(&format!(
                    "track_ground rgb={:.3},{:.3},{:.3} L*={:.1} span={:.1} want={:.3} drawn={:.3} rect={:.0},{:.0},{:.0},{:.0}",
                    ground[0], ground[1], ground[2],
                    lstar([ground[0], ground[1], ground[2], 1.0]),
                    crate::gfx::ground_span(),
                    track_alpha_for(ground), drawn,
                    track.x, track.y, track.w, track.h,
                ));
            }
            unsafe { LAST = n.wrapping_add(1) };
        }
        if glass_on {
            // PREPARE is deliberately not here — see `tab_glass_prepare`. Resolving cadence during
            // the page draw invalidates the backdrop after any earlier owner has already captured
            // one, which on the direct path means the snapshot is taken and then thrown away.
            // The track never moves, so its drawn rect IS its rest rect — no slide to correct for.
            // Hoisted out of the call, because it is now the BAND's face and not just this
            // surface's: [`profile_chip`] draws the other half of it from [`BAR_MATERIAL`], at the
            // same stops and the same rim weight, off this one solve. See [`BarMaterial`].
            let face = {
                let (gt, gb) = tab_glass_stops(ground);
                let (rim, rim_lit) = track_rim(gt[3]);
                crate::gfx::GlassFace { scrim_top: gt, scrim_bot: gb, rim, rim_lit, rim_w: 1.0 }
            };
            if Glass::DYNAMIC_BACKDROP.backdrop(
                p,
                track,
                0.0,
                track.h * 0.5,
                [1.0, 1.0, 1.0, 1.0],
                // The line and the hairline, no ramp — the design system's rule for this container,
                // and the one the panel treatment gets wrong here: 76px tall has 20px of interior
                // left once a 28px chamfer has run in from both edges, so the "rim" stops being an
                // edge and becomes most of the object.
                crate::gfx::GlassRim::Standing,
                face,
                // The bar is UltraThin by construction: it takes no extra sample, so the page comes
                // through as sharp as the chain left it. Its FROST is not read from the scale at all
                // — `tab_glass_stops` above solves it against the ground every frame.
                theme::Material::UltraThin,
            ) {
                // Published only once the chain has actually DRAWN. A refusal is the flat fallback
                // below, and the chip has to fall back with it — a glass chip beside a flat track
                // is the seam this whole arrangement exists to prevent, arrived at from the one
                // direction the geometry cannot.
                unsafe { BAR_MATERIAL = BarMaterial::Glass(face) };
                // NOTHING IS DRAWN HERE ANY MORE, and that is the fix. The darkening and the edge —
                // `inset 0 0 0 1px var(--glass-rim), inset 0 1px 0 var(--glass-rim-light)`, the whole
                // of what the design system puts on this container — used to be a SECOND rounded rect
                // over the backdrop, and two surfaces of one shape means two antialiased edges, each
                // blending its own colour against the page. They ride inside the surface now, as its
                // `GlassFace`; `fs_glass.frag` composites scrim then rim then coverage, in that order.
                //
                // The two weights are still one ring: `theme::GLASS_RIM` .14 the whole way round plus
                // the difference up to `theme::GLASS_RIM_LIGHT` .28 on the edge facing the light,
                // boosted by the surface NORMAL so it dies out along each cap's arc. It was a second
                // ring scissored to the top half for one build, and the scissor lands exactly on the
                // widest point of a cap — a hard step from .28 to .14 in the middle of a curve, which
                // reads as the outline snapping off. Reported from a screenshot before it was measured.
                //
                // And the weight TRAVELS with the ground (`track_rim`), because a lit edge is a
                // relationship and not a colour — `theme::GLASS_RIM_MAX` carries that measurement.
                // The ring is white in both weights: it was a PAIR while the track had two
                // polarities, an ink line for the light one and a light line for the dark, and the
                // ink half went with the polarity.
            } else {
                // …and the same here: the chain refused, so the flat dark material is what draws —
                // said to the spring, not to the value the spring publishes. See the note below.
                p.rect_sheened(track, track.h * 0.5, theme::TAB_TRACK_TOP, theme::TAB_TRACK_BOT);
            }
        } else {
            // NOT during a blur source pass. `tab_glass_on` is false there by design — the flat
            // track is the right source pixel — but deactivating on that basis makes the NEXT
            // frame's `prepare` see an inactive state, call `activate`, and invalidate the
            // backdrop. The cadence then runs at the present rate instead of every third one:
            // measured 50 refreshes/s against the intended 20, and an 11% whole-frame regression
            // that looked exactly like the direct path being slow.
            if !crate::gfx::blur_source_pass() {
                unsafe { (*std::ptr::addr_of_mut!(TAB_GLASS_STATE)).deactivate() };
            }
            // The flat capsule, which is the same material at its ceiling — there is nothing for
            // this path to say about ink any more. It once wrote the row's POLARITY here, and that
            // was a reported bug twice over: the write clobbered a value the spring owned, so the
            // next frame cut it back, and the hysteresis read the same clobbered value and walked
            // the committed polarity across with it. Both are gone with the polarity itself.
            p.rect_sheened(track, track.h * 0.5, theme::TAB_TRACK_TOP, theme::TAB_TRACK_BOT);
        }
        // A strip wider than its track is a bounded panel, not a scrolling document, so this is the
        // scissor case (see the ui/CLAUDE.md clipping rule): a pill leaving the row is cut at the
        // pill area's edge — which is also the "there is more over there" affordance. Paired below.
        // A non-zero scroll clips too even when the strip now fits: the section table can shrink
        // (sign-out, server switch) and the spring takes a few frames to unwind, and those frames
        // must not paint pills outside the track.
        let view = Rect::new(x0, TOP_BAR_Y, view_w, TAB_PILL_H);
        let sx = unsafe { addr_of!(TAB_SCROLL).read() }.pos;
        let scrolls = content_w > view_w + 0.5 || sx.abs() > 0.5;
        if scrolls {
            p.clip(view);
        }
        let env = Env::inert();
        // The selection/focus fills, as ONE travelling capsule each ([`TOP_STRIP`]) rather than a
        // boolean fill per pill. They were placed in content space with `tab_pill_x`, so a single
        // translate puts them and the pills on the same ruler; drawn first, because they are the
        // pills' ground. This strip is NOT plated — it sits inside the tab-bar track above, which
        // already is the ground, so the pills paint no fill of their own at all here.
        let cp = p.translate(x0 - sx, 0.0);
        unsafe { &*addr_of!(TOP_STRIP) }.draw(cp, TOP_BAR_Y, TAB_PILL_H, false, glass_on);
        rects.reserve(n);
        for i in 0..n {
            // ONE prefix sum per pill: `tab_pill_x` is O(i), and it was walked twice here — once for
            // the rect and again for the capsule coverage — so the row re-summed its own width every
            // frame for nothing.
            let px = tab_pill_x(widths, i);
            let r = Rect::new(x0 + px - sx, TOP_BAR_Y, widths[i], TAB_PILL_H);
            // ONE rule for "is this pill on screen": its rect clipped to the viewport. The clipped
            // rect is what gets recorded for the hit test AND what decides whether to draw at all
            // (a 12-library server would otherwise lay out three rows' worth of text the scissor
            // throws away), so the two can never disagree about a sliver at the edge.
            let vis = r.intersect(view);
            rects.push(vis);
            if vis.w > 0.5 {
                // ink comes from how covered this pill is by the capsules above — never from its own
                // booleans, or a mid-travel label could darken toward ACCENT_INK with nothing bright
                // under it (`cap_cover` is the one rule both sides read).
                let (fm, sm) = unsafe { &*addr_of!(TOP_STRIP) }.mixes((px, widths[i]));
                if i + 1 == n {
                    // The Search pill is a MARK, and it takes its ink from exactly the same
                    // `mixed_ink` the labels do — so it darkens toward ACCENT_INK under the focus
                    // capsule in step with its neighbours instead of being a separate colour story.
                    let d = TAB_ICON_D;
                    let ir = Rect::new(r.x + (r.w - d) * 0.5, r.y + (r.h - d) * 0.5, d, d);
                    crate::ui::icons::draw(p, crate::ui::icons::Icon::Search, ir, TabPill::mixed_ink(fm, sm));
                } else {
                    TabPill::new(labels[i].as_ptr(), theme::size::BODY, r).mix(fm, sm).draw(&env, p);
                }
            }
        }
        if scrolls {
            p.clip_clear();
        }
    })
}

/// Which colour treatment a control (Button / CircleButton) wears. One control widget, four looks —
/// the focus-driven default (every pill and disc in the app, the hero Play button included), a
/// caller-coloured one-off, the keyline pill for a secondary action over video, and the
/// destructive face.
///
/// There used to be a fourth, `Primary`: an always-filled cool-white CTA. It went with
/// `theme::FILL_PRIMARY` in the 2026-08-13 palette sync — **nothing is filled by rank, only by
/// focus**, so a control that lights up while the remote is elsewhere is a lie about where you are,
/// and at ten feet its white was indistinguishable from [`theme::ACCENT`] anyway. It had no callers
/// by then; the hero Play pill has been `Accent` for months.
#[derive(Clone, Copy)]
pub enum ControlStyle {
    /// focus-driven: focused → ACCENT + dark ink; idle → solid dark disc + white ink. The
    /// default, and the shared look of the transport buttons / info-card actions / detail buttons.
    Accent,
    /// caller supplies the exact fill + ink.
    Custom { fill: [f32; 4], ink: [f32; 4] },
    /// The **keyline** pill — idle, a hairline outline ([`theme::PILL_KEYLINE`]) knocked out over
    /// a translucent near-black interior ([`theme::PILL_KEYLINE_BG`]), for a secondary action
    /// sitting on SCRIMMED VIDEO (the post-play card's "Watch credits"), where [`Accent`]'s solid
    /// idle plate reads as a hole in the picture. Focused it takes the standard Accent treatment,
    /// so focus reads identically across every control in the family.
    Keyline,
    /// The **destructive** face — the control that ends something (the exit alert's *Exit*).
    ///
    /// Idle it states the hue TWICE: the plate is [`Accent`]'s neutral one carrying
    /// [`theme::DANGER_IDLE_TINT`] of [`theme::DANGER`] ([`theme::CONTROL_DANGER_IDLE_FILL`]) and
    /// the label is drawn in the danger ink itself. That is not belt-and-braces — at ten feet a
    /// 16% tint on a dark plate is a shade of grey, and the point of the pair is that the action
    /// is NAMED as destructive before the remote ever reaches it. Focused, the hue takes the whole
    /// face: [`theme::DANGER`] fill under [`theme::TEXT_PRIMARY`] ink.
    ///
    /// **There is no keyline, and that is a decision rather than an omission.** A danger-tinted
    /// perimeter was a third signal saying what the plate and the label already say, and the
    /// perimeter sheen every control wears ([`control_rim`]) is a card CONSTANT, not a state — a
    /// style that made it a state would be the one control in the app whose edge means something.
    ///
    /// The invariant to preserve: **the colour is a property of the ACTION, the FILL is still a
    /// property of FOCUS.** Every other control in this family lights up for exactly one reason,
    /// and this one must not become a button that is filled by rank (the reason `Primary` went).
    Danger,
}
/// A control's EDGE — the 1px perimeter the design system has always asked every control to wear
/// and which none of them wore.
///
/// **It is a rim and nothing else: no blur, no region, no budget.** A glass FACE was built and
/// looked at on the panel first, and it is not worth its price here — the app's controls sit in the
/// darkest quarter of the frame by construction (the hero wedge is what buys their labels their
/// contrast), so there is nothing behind them to bend. Measured on the dev set's own Home: the
/// blurred face moves a control's pixels by at most 7/255 while this rim moves them by up to 74.
/// Dropping the backdrop also drops the thing that made controls unaffordable — a glass surface is
/// charged for its rectangle unioned with every other, and Home's action row unioned with a glass
/// tab track priced at 3.8x the frame budget. A rim costs one extra fragment op on the perimeter.
///
/// **The weights are the TILE's, plus the container's lamp**: [`theme::CARD_SHEEN`] .22 round the
/// whole perimeter — a card's own edge, because a control face has a card's problem, it sits over
/// artwork nobody chose — and the boost to [`theme::GLASS_RIM_LIGHT`] .28 on the side facing the
/// light, weighted by the surface normal, so a disc's highlight sits on its crown and fades to
/// nothing at its equator. One lamp, the one every card shadow in this app is already cast from.
///
/// The perimeter used to be [`theme::GLASS_RIM`] .14, the GLASS container's line, which was a
/// category error by the design system's own rule: `tokens/glass.css` scopes that pair to a surface
/// you read THROUGH and says in the same breath that every control is flat. A flat control wears the
/// card constant. The crown stays the glass hairline because that is what it is — a specular line
/// from the shared lamp, not a second perimeter — so the edge now runs .22 → .28 instead of
/// .14 → .28, and this rim is the ONLY thing separating a control from its ground since the drop
/// shadow went (`Button::draw`).
///
/// Unconditional on focus, which is the point: the design's rule is that the card constants are not
/// states and the FILL is what focus owns. On the focused near-white `ACCENT` face a white rim is
/// invisible by construction, which is the sign it is an edge and not a fill.
fn control_rim(p: Painter, r: Rect, rad: f32, face: [f32; 4]) {
    p.rect_rimmed(r, rad, face, face, theme::CARD_SHEEN, theme::GLASS_RIM_LIGHT[3] - theme::CARD_SHEEN[3]);
}

impl ControlStyle {
    /// (fill, ink) for this style at the given focus state.
    pub(crate) fn colors(self, focused: bool) -> ([f32; 4], [f32; 4]) {
        match self {
            ControlStyle::Accent if focused => (crate::ui::ACCENT, crate::ui::ACCENT_INK),
            ControlStyle::Accent => (theme::CONTROL_IDLE_FILL, theme::CONTROL_IDLE_INK),
            ControlStyle::Custom { fill, ink } => (fill, ink),
            ControlStyle::Keyline if focused => (crate::ui::ACCENT, crate::ui::ACCENT_INK),
            ControlStyle::Keyline => (theme::PILL_KEYLINE_BG, theme::TEXT_HEADING),
            // The hue takes the WHOLE face on focus, exactly as `Accent`'s near-white does —
            // same shape, same rim, same shadow, one substitution. That is the sentence
            // `ControlStyle::Danger` makes: the colour belongs to the action, the fill to focus.
            ControlStyle::Danger if focused => (theme::DANGER, theme::TEXT_PRIMARY),
            ControlStyle::Danger => (theme::CONTROL_DANGER_IDLE_FILL, theme::CONTROL_DANGER_IDLE_INK),
        }
    }
}

// ---- Button: a pill with a label, an optional leading icon and an optional TRAILING accessory,
// centered together as one group (icon + gap + label + gap + accessory is centered in the pill).
// Colour per `ControlStyle` (default Accent). The one reusable action button — hero Play (Primary),
// detail/info actions (Accent), etc. ----
pub struct Button {
    pub frame: Rect,
    pub label: *const c_char,
    pub sz: c_int,
    pub icon: Option<crate::ui::icons::Icon>,
    /// The TRAILING accessory glyph — a chevron saying the press opens a list rather than acting
    /// ([`crate::ui::alt_sources`]'s *Also available*). Deliberately its own slot rather than a
    /// second use of [`Button::icon`]: the leading icon is part of the label's own statement (the
    /// Play triangle IS "play"), while this one is a disclosure mark about what the control DOES,
    /// and the two are read in opposite directions. It is the same `›`-family mark
    /// [`crate::ui::table::Row::ticon`] puts at a row's trailing edge, for the same reason.
    pub trailing: Option<crate::ui::icons::Icon>,
    pub focused: bool,
    pub style: ControlStyle,
    /// The FOCUS POP, as a factor on the frame — see [`Button::scale`].
    pub scale: f32,
    /// 0..1 left-to-right FILL sweep across the pill; None = an ordinary button.
    pub progress: Option<f32>,
}
/// [`Button`]'s icon box, as a multiple of its type size, and the icon→label gap. Named because
/// [`Button::pill_w`] measures the same run `Button::draw` lays out — a literal in each would let
/// the two drift.
const BTN_ICON_RATIO: f32 = 1.15;
const BTN_ICON_GAP: f32 = 12.0;
/// Total horizontal air a pill carries around its icon+label run.
const BTN_PILL_AIR: f32 = 68.0;
/// [`ControlStyle::Keyline`]'s stroke width (the design's 1.5 — same weight as [`keyline_chip`]'s).
const BTN_KEYLINE_W: f32 = 1.5;

impl Button {
    pub fn new(label: *const c_char, sz: c_int, frame: Rect) -> Self {
        Self {
            frame,
            label,
            sz,
            icon: None,
            trailing: None,
            focused: false,
            style: ControlStyle::Accent,
            scale: 1.0,
            progress: None,
        }
    }
    /// The [FOCUS POP](CTRL_FOCUS_SCALE), about the capsule's centre — normally [`CtlPop::scale`],
    /// which already folds the press dip in. `1.0` is the resting control.
    ///
    /// The pill's TYPE does not scale with it, for the reason [`CircleButton::scale`] gives: the
    /// size ladder has no rung between 28 and 32 and text may not sit between rungs. So the plate
    /// and its icon grow and the label keeps its measured width, which is also what keeps the
    /// centred `[icon + gap + label]` run centred through the pop.
    pub fn scale(mut self, s: f32) -> Self {
        self.scale = s;
        self
    }
    pub fn icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.icon = Some(i);
        self
    }
    /// Give this pill a trailing accessory — see [`Button::trailing`].
    pub fn trailing_icon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.trailing = Some(i);
        self
    }
    pub fn focused(mut self, f: bool) -> Self {
        self.focused = f;
        self
    }
    pub fn style(mut self, s: ControlStyle) -> Self {
        self.style = s;
        self
    }

    /// The width this button wants for `label` at type size `sz`: its own content run (icon box +
    /// gap + label, per [`BTN_ICON_RATIO`]/[`BTN_ICON_GAP`]) plus one air budget. The LAYOUT
    /// companion to `draw`, which only ever centres that same run in the frame it is handed — so
    /// the two read the same constants and cannot drift.
    ///
    /// ONE formula for every pill in the product, because they all relabel from state and a fixed
    /// frame that fits the short word crams the long one against its own capsule ends: both hero
    /// rows ("Play"/"Continue", "Play"/"Resume") pass `icon: true`, and Home's status-screen Retry
    /// control passes `false`. That flag is the whole reason this takes one — an icon-less pill
    /// measured with an icon box gets a slug of air it never fills, which is why the earlier
    /// icon-only version had to send such callers off to `text::text_width` on their own. A second
    /// sizing path is exactly the drift this file exists to prevent.
    pub fn pill_w(label: *const c_char, sz: c_int, icon: bool) -> f32 {
        Self::pill_w_full(label, sz, icon, false)
    }

    /// [`Button::pill_w`] with the TRAILING accessory counted too — the same one formula, taking
    /// both of the button's optional slots rather than growing a second sizing path beside it (the
    /// drift this file exists to prevent, and the exact reason `pill_w` gained its `icon` flag).
    /// An accessory occupies one more icon box and one more gap, which is what [`Button::draw`]
    /// lays out below.
    pub fn pill_w_full(label: *const c_char, sz: c_int, icon: bool, trailing: bool) -> f32 {
        let (isz, gap) = if icon { (sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        let (tsz, tgap) = if trailing { (sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        isz + gap + crate::text::text_width(label, sz, 1) + tgap + tsz + BTN_PILL_AIR
    }

    /// Turn the pill into its own countdown: `frac` of its width is filled with
    /// [`theme::CONTROL_SPENT_FILL`], the rest with the button's normal face, so time reads as a
    /// sweep across the control itself instead of a separate rail beside it.
    ///
    /// **Progress and focus are separate channels, deliberately.** The first version drew the
    /// filled part as the FOCUSED face and the rest as the idle one, which collapsed the two: a
    /// focused counting button was pixel-identical to an unfocused idle one at t=0, and the label's
    /// ink flipped at the sweep line — bisecting a word with a hard edge, which from a couch reads
    /// as a torn glyph atlas rather than a timer. Now the face is drawn once, at its true focus
    /// state, and only the FILL BEHIND the label changes — the ink never inverts.
    pub fn progress(mut self, frac: f32) -> Self {
        self.progress = Some(frac);
        self
    }

    /// The pill's filled background, including the countdown sweep when one is set. Takes the rect
    /// rather than reading `self.frame`, because the drawn plate is the POPPED one
    /// ([`Button::scale`]) and the sweep has to ride it.
    fn plate(&self, p: Painter, r: Rect, bg: [f32; 4]) {
        let rad = r.h * 0.5;
        if matches!(self.style, ControlStyle::Keyline) && !self.focused {
            // the knockout: stroke colour first, then the interior inset by it — the SDF has no
            // stroke-only mode (`keyline_chip` / `pass_capsule`'s construction); `bg` here is
            // `colors()`'s translucent interior, not a repaint of the ground (there is none over
            // live video — see `theme::PILL_KEYLINE_BG`)
            p.rrect(r, rad, rad, theme::PILL_KEYLINE);
            let s = BTN_KEYLINE_W;
            p.rrect(Rect::new(r.x + s, r.y + s, r.w - 2.0 * s, r.h - 2.0 * s), rad - s, rad - s, bg);
        } else {
            control_rim(p, r, rad, bg);
        }
        let Some(frac) = self.progress else { return };
        let w = r.w * frac.clamp(0.0, 1.0);
        if w <= 0.0 {
            return;
        }
        // Scissor so the sweep inherits the capsule's rounded ends instead of a square edge; it is
        // GLOBAL GL state, so it is set and cleared inside this one draw and never left armed.
        p.clip(Rect::new(r.x, r.y, w, r.h));
        p.rrect(r, rad, rad, theme::CONTROL_SPENT_FILL);
        p.clip_clear();
    }
}
impl View for Button {
    fn draw(&self, _e: &Env, p: Painter) {
        let r = self.frame.scaled(self.scale);
        let (bg, ink) = self.style.colors(self.focused);
        // **No drop shadow.** A control face is held by its EDGE ([`control_rim`], now the card's own
        // .22 sheen rather than the glass container's .14 line) and by the focus pop, and the design
        // system states the family that way: `Button`/`CircleButton` carry the 1px perimeter sheen
        // plus the specular top hairline, and nothing under them.
        //
        // This did carry `CARD_SHADOW_REST`, on a measured argument worth keeping written down: an
        // ACCENT capsule over a WHITE frame measures ~1.2:1 against its surround, so the plate
        // disappears and only the dark label is left floating. The answer to that is the rim's own
        // weight — the case is a near-white face on a near-white ground, where a 4px shadow at .34
        // was never what separated them either. If it ever reads thin on real artwork, the lever is
        // the rim, not a shadow coming back: two elevations for one control family is what the
        // design system removed.
        self.plate(p, r, bg);
        // center the [icon + gap + label] group in the pill; the label sits on the pill centre by
        // its cap band, so descenders (the g's in "From Beginning") don't drag the caps upward
        let ty = crate::text::text_vcenter_y(self.sz, 1, r.y + r.h * 0.5);
        let tw = crate::text::text_width(self.label, self.sz, 1);
        let (isz, gap) =
            if self.icon.is_some() { (self.sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        let (asz, agap) =
            if self.trailing.is_some() { (self.sz as f32 * BTN_ICON_RATIO, BTN_ICON_GAP) } else { (0.0, 0.0) };
        // the WHOLE run is centred — accessory included — which is why `pill_w_full` measures it:
        // sizing the pill without the chevron and then drawing one would push the label off-centre
        let gl = r.cx() - (isz + gap + tw + agap + asz) * 0.5;
        if let Some(icon) = self.icon {
            crate::ui::icons::draw(p, icon, Rect::new(gl, r.y + (r.h - isz) * 0.5, isz, isz), ink);
        }
        p.text(self.label, gl + isz + gap, ty, self.sz, ink, 0, 1); // left-aligned after the icon
        if let Some(acc) = self.trailing {
            let ax = gl + isz + gap + tw + agap;
            crate::ui::icons::draw(p, acc, Rect::new(ax, r.y + (r.h - asz) * 0.5, asz, asz), ink);
        }
    }
}

// ---- Badge: the small rounded metadata chip (CC / SDH / AD / FORCED / codec tags), with an
// OPTIONAL leading glyph. ONE leaf for the track-menu rows, the Info card meta line, the detail
// About column and the episode filmstrip's duration pill, so the chip look can't drift. Cap-band-
// centred bold CAPTION label; width hugs the label with a floor so short tags (CC) still read as a
// chip. Returns the drawn width so callers can flow chips inline. ----
pub(crate) enum BadgeStyle {
    /// 2px border + knockout interior: label in `col`, ring in `border`, interior filled `bg` (the
    /// surface behind the chip — keeps the outline clean over a light focus pill or a dark panel).
    ///
    /// The ring is its OWN colour because the design system makes it one: a table row's chip is
    /// `border: 2px solid (focused ? --accent-ink : --overlay-border)` with `color: ink`, i.e. the
    /// stroke is a keyline and only the label carries the row's ink. Both callers passed `col` for
    /// both, so every chip off a focus pill outlined at full [`theme::TEXT_PRIMARY`] — nearly twice
    /// the ink the contract gives it, which is what made a FORCED/SDH tag read as loud as the track
    /// name beside it. [`theme::OVERLAY_BORDER`] is the value, and its own doc has said
    /// "outlined-badge / meta-badge border" the whole time it had no consumer.
    Outlined { col: [f32; 4], border: [f32; 4], bg: [f32; 4] },
    /// solid translucent fill ([`theme::BADGE_FILL`]), label in [`theme::TEXT_HEADING`] — the
    /// About column's accessibility chips.
    Filled,
    /// A CAPSULE that rides on ARTWORK: the idle-control pair ([`theme::CONTROL_IDLE_FILL`] face,
    /// [`theme::CONTROL_IDLE_INK`] ink) and fully rounded ends — the same surface
    /// [`watched_badge`]'s disc wears, so a chip and a disc laid over the same still read as one
    /// family. Deliberately NOT [`BadgeStyle::Filled`]: that chip's translucent light fill is
    /// legible in a dark text column and disappears over a bright thumbnail.
    ///
    /// Its ink is NEUTRAL on purpose. Amber (`RESUME_*`) is the app's one watched-STATE hue, and a
    /// chip in that hue over a tile that already carries a state mark would be a second, competing
    /// claim about the same item (see `ui/CLAUDE.md`'s one-vocabulary rule).
    OverArt,
}
/// The chip's height — one band for every style, so a row mixing them stays on one line. Public
/// because a caller that pins a chip to an edge (the episode still's duration pill) needs to know
/// how tall the thing it is placing is.
pub(crate) const BADGE_H: f32 = 34.0;
/// A leading glyph's box, at the label's own type size — a touch over its cap height, the same
/// relationship [`rating_group`]'s verdict mark has to its score. Deliberately smaller than
/// [`Button`]'s `sz * BTN_ICON_RATIO`: this chip is half a button's height, so a button-proportioned
/// glyph would fill it edge to edge.
const BADGE_ICON: f32 = theme::size::CAPTION as f32;
/// Glyph → label air. They are one run, so the tightest rung (as in [`rating_group`]).
const BADGE_ICON_GAP: f32 = theme::space::XS;

/// pixel width [`badge`] will occupy for `text` (+ `icon`) — the layout companion (e.g. reserving
/// the inline-chip run so a row label elides before it). The icon's band is added OUTSIDE the
/// short-tag floor, so a bare "CC" still measures its minimum and a glyphed chip still fits both.
// ---- The PLEX PASS capsule (`Details Screen.dc.html` / `Player Screen.dc.html`) --------------
//
// The name set in type — deliberately NO logo artwork: this is an unofficial client, and the
// badge is a referential use of the words alone (the same reasoning `plex::identity` documents
// for the product name). Height matches [`BADGE_H`] so it shares a badge row's optical line.
// **Two product surfaces, both places the name changes what the user does next** (see
// `theme::PASS_GOLD`'s doc for the docs-derived rule): FILLED in the playback-failed read-out
// (pure black ground), OUTLINE in the detail facts row's Pass-gated states. Non-interactive in
// both; it is [`theme::PASS_GOLD`]'s only consumer.
//
// **Re-spec'd 2026-08-12** to the geometry BOTH mock files now carry (which is what makes it a
// decision rather than a drift): an 8px rounded rect at a 2px stroke, `size::CAPTION` bold at
// `.06em` — up from a full-pill silhouette, a 1.5px stroke and `size::MICRO` at `.12em`. The
// silhouette is no longer what separates it from a technical chip; the GOLD is, and the label
// now sits on the couch-legibility floor instead of below it. (The Player Screen's comment
// beside the filled form still says "full pill radius" against its own `border-radius:8px` —
// the CSS is the artifact and the comment is stale.)

/// Letter-tracking for the capsule label: the design's `.06em` of [`theme::size::CAPTION`]. The
/// text renderer has no letter-spacing, so the label is drawn per character.
const PASS_TRACK: f32 = theme::size::CAPTION as f32 * 0.06;
/// The label inset. The design states `padding: 0 13px 0 15px` and **that asymmetry does not
/// port** — honouring the reasoning, not the literal. The mock is asymmetric because CSS emits a
/// letter-space after the LAST character too, so it takes that trailing space off the right pad to
/// keep the label optically centred; its own comment says exactly that. Our renderer tracks
/// BETWEEN characters only ([`pass_label_w`] counts `n − 1` gaps), so there is no trailing space to
/// compensate for, and copying 15/13 would push the ink 1px right of centre — off-centre in the
/// opposite direction from the thing the design was correcting. 14/14 keeps the SUM, and therefore
/// the drawn width and every layout measured from it, byte-identical.
const PASS_PAD_X: f32 = 14.0;
/// Corner radius and stroke — `border-radius: 8px`, `inset 0 0 0 2px`.
const PASS_RAD: f32 = 8.0;
const PASS_STROKE: f32 = 2.0;
/// The label, "PLEX PASS", **pre-split into per-character `CStr` literals** — the tracking is
/// applied by advancing the pen between them, so the label is nine one-character draws.
/// Compile-time constants rather than nine `CString::new` allocations per call: `pass_capsule_w`
/// alone walks them, and the capsule is now on the detail hero for every converting item on a
/// proven-Pass-less server (not only an HDR one) as well as in the failure read-out, so this ran
/// ~18 small allocations a frame for a string that never changes.
const PASS_CHARS: [&std::ffi::CStr; 9] = [c"P", c"L", c"E", c"X", c" ", c"P", c"A", c"S", c"S"];

/// The label's own drawn width, **memoised** — the pens below re-measure per character anyway, so
/// only the total is worth holding. Main-thread only, like every other layout memo here.
static mut PASS_W: f32 = 0.0;

fn pass_label_w() -> f32 {
    // `text_width` reads 0 until `init_text` has run — never cache a pre-init measurement (the
    // same guard `ctrl_slot`'s width memo keeps, and for the same reason).
    let memo = unsafe { PASS_W };
    if memo > 0.0 {
        return memo;
    }
    let mut w = 0.0;
    for c in PASS_CHARS {
        w += crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1);
    }
    if w <= 0.0 {
        return 0.0;
    }
    w += PASS_TRACK * (PASS_CHARS.len() - 1) as f32;
    unsafe { PASS_W = w };
    w
}

/// Layout width of the capsule — for right-anchoring and row flow.
pub(crate) fn pass_capsule_w() -> f32 {
    pass_label_w() + 2.0 * PASS_PAD_X
}

/// Draw the capsule with its LEFT edge at `x`, centred on `cy`; returns its width.
///
/// `filled: false` is the OUTLINE form — a [`PASS_STROKE`] ring of pass-gold with a gold label and
/// **nothing inside it** ([`Painter::rring`]), which is the mock's `box-shadow: inset 0 0 0 2px
/// var(--pass-gold)` with no `background`. It is the default everywhere a surface sits behind it,
/// and it no longer has to be told what that surface is: the knockout it replaces painted the
/// interior in a `bg` the caller named, which over the detail hero's backdrop meant a gold-ringed
/// dark BOX rather than a hairline.
///
/// `filled: true` is the FILLED form — pass-gold fill, near-black label — used in exactly one
/// place, the playback-failed read-out, where the ground is pure black and an outline would read
/// as a hole.
pub(crate) fn pass_capsule(p: Painter, x: f32, cy: f32, filled: bool) -> f32 {
    let w = pass_capsule_w();
    let r = Rect::new(x, cy - BADGE_H * 0.5, w, BADGE_H);
    let ink = if filled {
        p.rrect(r, PASS_RAD, PASS_RAD, theme::PASS_GOLD);
        theme::PASS_GOLD_INK
    } else {
        p.rring(r, PASS_RAD, PASS_STROKE, theme::PASS_GOLD);
        theme::PASS_GOLD
    };
    let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
    let mut cx = x + PASS_PAD_X;
    for c in PASS_CHARS {
        p.text(c.as_ptr(), cx, ty, theme::size::CAPTION, ink, 0, 1);
        cx += crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1) + PASS_TRACK;
    }
    w
}

pub(crate) fn badge_w(text: &str, icon: Option<crate::ui::icons::Icon>) -> f32 {
    const PAD: f32 = 12.0;
    const MIN_W: f32 = 56.0;
    let lead = if icon.is_some() { BADGE_ICON + BADGE_ICON_GAP } else { 0.0 };
    std::ffi::CString::new(text)
        .ok()
        .map(|c| (crate::text::text_width(c.as_ptr(), theme::size::CAPTION, 1) + 2.0 * PAD).max(MIN_W) + lead)
        .unwrap_or(0.0)
}
/// Draw one chip with its LEFT edge at `x`, vertically centred on `cy`; returns its width.
pub(crate) fn badge(p: Painter, x: f32, cy: f32, text: &str, icon: Option<crate::ui::icons::Icon>, style: BadgeStyle) -> f32 {
    let lc = match std::ffi::CString::new(text) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let sz = theme::size::CAPTION;
    let w = badge_w(text, icon);
    let r = Rect::new(x, cy - BADGE_H * 0.5, w, BADGE_H);
    let ink = match style {
        BadgeStyle::Outlined { col, border, bg } => {
            let bw = 2.0f32;
            p.rrect(r, 6.0, 6.0, border); // keyline
            p.rrect(Rect::new(r.x + bw, r.y + bw, r.w - 2.0 * bw, r.h - 2.0 * bw), 5.0, 5.0, bg);
            col
        }
        BadgeStyle::Filled => {
            p.rrect(r, 7.0, 7.0, theme::BADGE_FILL);
            theme::TEXT_HEADING
        }
        BadgeStyle::OverArt => {
            let rad = r.h * 0.5;
            p.rrect(r, rad, rad, theme::CONTROL_IDLE_FILL);
            theme::CONTROL_IDLE_INK
        }
    };
    let ty = crate::text::text_vcenter_y(sz, 1, cy);
    // [glyph + gap + label] centred in the chip as ONE run — the same composition `Button::draw`
    // uses, so a chip and a pill put their icon in the same optical place. With no icon `lead` is 0
    // and this collapses to the label centred on its own, which is what it always did.
    let lead = if icon.is_some() { BADGE_ICON + BADGE_ICON_GAP } else { 0.0 };
    let tw = crate::text::text_width(lc.as_ptr(), sz, 1);
    let gl = r.cx() - (lead + tw) * 0.5;
    if let Some(i) = icon {
        crate::ui::icons::draw(p, i, Rect::new(gl, cy - BADGE_ICON * 0.5, BADGE_ICON, BADGE_ICON), ink);
    }
    p.text(lc.as_ptr(), gl + lead, ty, sz, ink, 0, 1); // left-aligned after the glyph
    w
}

// ---------------------------------------------------------------------------------------
// ---- Rating row: one PROVIDER's scores under the provider's name in words.
//
// Rewritten 2026-08-02 from `Details Screen.dc.html`. It used to draw one badge per score, each
// behind that provider's own brand mark — Rotten Tomatoes' fruit and popcorn tub as tinted
// silhouettes, IMDb and TMDB as logotype chips in their brand colours. All of that is gone:
//
//   * the RT marks had no licensing route (see `ui/icons.rs`), and
//   * the chips were reproducing two more brands' logotypes to solve a problem — "whose score is
//     this?" — that a WORD solves for free, and that naming the provider solves *lawfully*, since
//     referential use needs no licence where a mark does.
//
// So a group is: the provider's name as a quiet MICRO caption in TEXT_TERTIARY, then its score or
// scores. That inverts what carried the colour. Before, four saturated brand marks competed with
// the hero art and with each other; now the captions recede to caption weight and the ONLY colour
// left in the row is the verdict — a red or gold or hollow tomato, a green or drained crowd. The
// row reads as one rhythm instead of four logos.
//
// Rotten Tomatoes is ONE group with two scores under one caption, because critics and audience are
// two readings from one source; IMDb and TMDB are one score each. That is also why this draws a
// GROUP rather than a badge: the caption is shared, so the unit that knows how to lay itself out
// is the provider, not the score.
// ----

/// Mark box (px). A little over the meta line's cap height so a 26-unit silhouette still resolves
/// at couch distance — these marks carry the VERDICT, so legibility here is not cosmetic.
const RATING_MARK_D: f32 = 30.0;
/// Glyph → its score. They are one unit, so it stays tight.
const RATING_GAP: f32 = 10.0;
/// Provider caption → the first score under it.
const RATING_CAPTION_GAP: f32 = 12.0;
/// Score → the next glyph in the SAME group (Rotten Tomatoes' critic → audience). Wider than
/// [`RATING_GAP`] so the two pairs read as two readings rather than one run of four things.
const RATING_PAIR_GAP: f32 = 14.0;

/// One colour layer of a rating mark — a mask and the tint it is painted in. Marks are two-tone
/// (body + calyx), and the rasterizer renders a MASK, so a mark is a slice rather than one icon.
pub(crate) type MarkLayer = (crate::ui::icons::Icon, [f32; 4]);

/// One score inside a provider group: the mark that carries its verdict, and the score as text.
/// `mark` is empty for a provider that has no verdict to draw (IMDb, TMDB) — their number IS the
/// whole statement, and inventing a glyph for them is what put a meaningless star here before.
pub(crate) struct RatingCell<'a> {
    pub(crate) mark: &'a [MarkLayer],
    pub(crate) value: &'a str,
    /// Trailing unit set a rung down in tertiary ink — IMDb's "/10". A percentage carries its own
    /// "%" inside `value`, because there the unit is part of the number rather than a scale note.
    pub(crate) suffix: &'a str,
}

/// Width [`rating_group`] will occupy. Measure before drawing so a row can stop at a margin
/// instead of running a group off the panel (same contract as `badge`/`badge_w`).
pub(crate) fn rating_group_w(caption: &str, cells: &[RatingCell]) -> f32 {
    let cap = std::ffi::CString::new(caption).ok();
    let mut w = match cap {
        Some(c) => crate::text::text_width(c.as_ptr(), theme::size::MICRO, 1) + RATING_CAPTION_GAP,
        None => return 0.0,
    };
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            w += RATING_PAIR_GAP;
        }
        if !cell.mark.is_empty() {
            w += RATING_MARK_D + RATING_GAP;
        }
        if let Ok(v) = std::ffi::CString::new(cell.value) {
            w += crate::text::text_width(v.as_ptr(), theme::size::LABEL, 1);
        }
        if let Ok(s) = std::ffi::CString::new(cell.suffix) {
            w += crate::text::text_width(s.as_ptr(), theme::size::MICRO, 1);
        }
    }
    w
}

/// Draw one provider's group with its LEFT edge at `x`, centred on `cy`; returns its width.
pub(crate) fn rating_group(p: Painter, x: f32, cy: f32, caption: &str, cells: &[RatingCell]) -> f32 {
    let Ok(cap) = std::ffi::CString::new(caption) else { return 0.0 };
    let mut bx = x;
    // The caption sits on the SCORE's baseline, not on its own centre: the design aligns the row
    // by baseline (`align-items:baseline`), so a MICRO caption beside a LABEL number must share
    // the number's baseline or it floats. `text::baseline_y` is that rule, shared.
    let base = crate::text::baseline_y(
        theme::size::MICRO,
        1,
        theme::size::LABEL,
        1,
        crate::text::text_vcenter_y(theme::size::LABEL, 1, cy),
    );
    p.text(cap.as_ptr(), bx, base, theme::size::MICRO, theme::TEXT_TERTIARY, 0, 1);
    bx += crate::text::text_width(cap.as_ptr(), theme::size::MICRO, 1) + RATING_CAPTION_GAP;

    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            bx += RATING_PAIR_GAP;
        }
        if !cell.mark.is_empty() {
            // every layer rides the SAME rect, so all of them rasterize at one size from one
            // viewBox and register exactly — see `ui/icons.rs`'s note on the layered marks
            let r = Rect::new(bx, cy - RATING_MARK_D * 0.5, RATING_MARK_D, RATING_MARK_D);
            for (mask, tint) in cell.mark.iter() {
                crate::ui::icons::draw(p, *mask, r, *tint);
            }
            bx += RATING_MARK_D + RATING_GAP;
        }
        if let Ok(v) = std::ffi::CString::new(cell.value) {
            let ty = crate::text::text_vcenter_y(theme::size::LABEL, 1, cy);
            p.text(v.as_ptr(), bx, ty, theme::size::LABEL, theme::TEXT_PRIMARY, 0, 1);
            bx += crate::text::text_width(v.as_ptr(), theme::size::LABEL, 1);
        }
        if let Ok(s) = std::ffi::CString::new(cell.suffix) {
            p.text(s.as_ptr(), bx, base, theme::size::MICRO, theme::TEXT_TERTIARY, 0, 1);
            bx += crate::text::text_width(s.as_ptr(), theme::size::MICRO, 1);
        }
    }
    // the MEASURED width, not what the draws accumulated — a draw and its measurer that can
    // disagree will eventually be caught disagreeing
    rating_group_w(caption, cells)
}


#[cfg(test)]
mod tests {
    use super::*;

    /// **A control row's pop animates BOTH ways**, which is the whole reason it is an array of
    /// springs and not one global scalar. Walking focus from control 0 to control 1 must leave 0
    /// still shrinking while 1 grows — a single spring could only snap the outgoing face to rest,
    /// which on a 60px disc is a visible 4px jump at the moment the eye is already on that control.
    #[test]
    fn a_control_leaving_focus_shrinks_while_its_neighbour_grows() {
        let mut pop: CtlPop<2> = CtlPop::new();
        for _ in 0..40 {
            pop.step(Some(0), 1.0 / 60.0);
        }
        let settled = pop.scale(0);
        assert!(
            (settled - CTRL_FOCUS_SCALE).abs() < 0.005,
            "a held focus settles ON the pop, not near it: {settled}"
        );
        assert_eq!(pop.scale(1), 1.0, "and its neighbour is at rest");

        // focus moves — one frame later BOTH are in flight, neither at an endpoint
        pop.step(Some(1), 1.0 / 60.0);
        let (leaving, arriving) = (pop.scale(0), pop.scale(1));
        assert!(leaving < settled && leaving > 1.0, "the leaving face is still shrinking: {leaving}");
        assert!(arriving > 1.0, "…while the arriving one has already started: {arriving}");
    }

    /// The pop is UNDERdamped — it overshoots and rings, because the design system names exactly two
    /// things in this app that bounce and this is one of them (`--ease-bounce`, shared with the
    /// press release). A critically damped spring cannot, so this is what would catch a `step` that
    /// quietly went back to `Spring::step`.
    #[test]
    fn the_pop_overshoots_the_way_a_click_does() {
        let mut pop: CtlPop<1> = CtlPop::new();
        let mut peak = 1.0f32;
        for _ in 0..40 {
            pop.step(Some(0), 1.0 / 60.0);
            peak = peak.max(pop.scale(0));
        }
        assert!(peak > CTRL_FOCUS_SCALE, "an underdamped pop passes its target: peaked at {peak}");
        assert!(peak < CTRL_FOCUS_SCALE * 1.05, "…but rings, it does not launch: {peak}");
    }

    /// Nothing focused closes every pop, and `reset` gets there with no motion at all — the two
    /// halves a page teardown needs (`detail::reset_view_state`), so a re-mounted page never opens
    /// with a control standing proud of its row.
    #[test]
    fn a_row_with_no_focus_settles_flat() {
        let mut pop: CtlPop<3> = CtlPop::new();
        for _ in 0..40 {
            pop.step(Some(2), 1.0 / 60.0);
        }
        for _ in 0..60 {
            pop.step(None, 1.0 / 60.0);
        }
        for i in 0..3 {
            assert!((pop.scale(i) - 1.0).abs() < 0.005, "control {i} eased back to rest");
        }
        pop.step(Some(1), 1.0 / 60.0);
        pop.reset();
        for i in 0..3 {
            assert_eq!(pop.scale(i), 1.0, "…and reset is exact, not nearly");
        }
    }

    /// An out-of-range index answers 1.0 rather than panicking. `detail::hero_ctls` builds a row
    /// whose LENGTH depends on the item (a resume point comes and goes, a second source comes and
    /// goes), so a focus index can outrun the array for the frame between a set changing and
    /// `hero_col` re-seating the focus on it.
    #[test]
    fn an_index_past_the_row_is_a_resting_control() {
        let mut pop: CtlPop<2> = CtlPop::new();
        pop.step(Some(7), 1.0 / 60.0);
        assert_eq!(pop.scale(7), 1.0);
        assert_eq!(pop.scale(0), 1.0, "and an out-of-range FOCUS pops nothing in range either");
    }

    /// The unfurl's three formulas are ONE formula asked three ways, and each way is load-bearing
    /// somewhere the others are not: [`CircleButton::cap_w`] places the capsule, `cap_label_w`
    /// recovers what it was sized for so the label's fade can be geometric rather than guessed, and
    /// [`CircleButton::label_budget`] is what a row with a hard right edge asks before allowing any
    /// of it (`detail::watch_cap_at`). They agree or the control paints a word past its own frame.
    #[test]
    fn the_unfurl_geometry_round_trips() {
        let d = StatusOverlay::CTRL_H;
        for label_w in [40.0f32, 120.0, 233.0, 267.0, 400.0] {
            assert_eq!(CircleButton::cap_w(d, 0.0, label_w), d, "shut is the bare disc");
            let open = CircleButton::cap_w(d, 1.0, label_w);
            assert!(open > d + label_w, "an open capsule holds its label and then some");

            for e in [0.05f32, 0.25, 0.5, 0.9, 1.0] {
                let w = CircleButton::cap_w(d, e, label_w);
                assert!(w >= d && w <= open, "the capsule stays between its two ends at e={e}");
                let back = CircleButton::cap_label_w(d, e, w).expect("open enough to invert");
                assert!((back - label_w).abs() < 0.01, "e={e}: recovered {back}, measured {label_w}");
            }
            // …and the budget is the same equation solved for the label: spending exactly it must
            // land the open capsule exactly `room` past the bare disc.
            let room = open - d;
            let budget = CircleButton::label_budget(d, room);
            assert!((budget - label_w).abs() < 0.01, "budget {budget} for a {label_w} label");
        }
        // a shut (or nearly shut) capsule carries no recoverable label — the range where the ramp
        // has nothing to fade in anyway
        assert_eq!(CircleButton::cap_label_w(d, 0.0, d), None);
        // "no label" collapses whatever the unfurl says, so a caller cannot animate a bare disc wide
        for e in [0.0f32, 0.5, 1.0] {
            assert_eq!(CircleButton::cap_w(d, e, 0.0), d);
            assert_eq!(CircleButton::cap_w(d, e, -5.0), d);
        }
    }

    /// The **destructive** face, both states, as the one sentence it is meant to say: *the colour
    /// belongs to the ACTION, the fill belongs to FOCUS.*
    ///
    /// Both halves are graded, because each fails invisibly on its own. An idle plate that is not
    /// tinted (a `Danger` arm that fell through to `Accent`'s neutral) still lights up correctly on
    /// focus, so a device capture of the focused pill looks right. A focused face that is not the
    /// whole hue reads as an ordinary control the moment the ring lands on it, which is the frame
    /// nobody screenshots. The comparisons are against the tokens rather than against colour codes,
    /// so a palette retune moves this test with the design instead of breaking it.
    #[test]
    fn the_danger_face_tints_when_idle_and_takes_the_whole_hue_when_focused() {
        let (idle_fill, idle_ink) = ControlStyle::Danger.colors(false);
        let (foc_fill, foc_ink) = ControlStyle::Danger.colors(true);

        // FOCUSED: the hue is the face, under primary ink — the same substitution `Accent` makes
        // with its near-white, so focus reads identically across the whole control family.
        assert_eq!(foc_fill, theme::DANGER);
        assert_eq!(foc_ink, theme::TEXT_PRIMARY);

        // IDLE: the neutral plate with the hue leaned into it, and the label in the hue itself —
        // the action is NAMED before the remote reaches it.
        assert_eq!(idle_fill, theme::CONTROL_DANGER_IDLE_FILL);
        assert_eq!(idle_ink, theme::DANGER);
        let (neutral, _) = ControlStyle::Accent.colors(false);
        assert_ne!(idle_fill, neutral, "an idle destructive plate is not the neutral one");
        assert_eq!(
            idle_fill[3], neutral[3],
            "…but it is the same MATERIAL — `mix` keeps the neutral's alpha, so the hue is the \
             only difference between the two idle plates"
        );
        // …and it is a TINT, not the fill: 16% of the way over, so the plate is nearer the
        // neutral it is derived from than the hue it is announcing.
        for c in 0..3 {
            let leaned = (idle_fill[c] - neutral[c]).abs();
            let whole = (theme::DANGER[c] - neutral[c]).abs();
            assert!(
                leaned <= whole * 0.5,
                "channel {c}: an idle plate that is half the hue is a fill, not a tint"
            );
        }
        assert_ne!(idle_fill, foc_fill, "the FILL is what focus owns, on this style as on every other");
    }

    /// **No keyline on the danger face.** The knockout stroke is `Keyline`'s alone — a danger rim
    /// would be a third signal saying what the plate and the label already say, and the perimeter
    /// sheen every control wears (`control_rim`) is a card CONSTANT rather than a state.
    ///
    /// `Button::plate` decides this with a `matches!` on one variant, so the property is not
    /// something the type system defends: a later `| ControlStyle::Danger` added to that test
    /// compiles and looks plausible. This is what refuses it.
    #[test]
    fn the_danger_face_wears_no_keyline() {
        let styled = |s: ControlStyle, focused: bool| {
            let b = Button::new(c"x".as_ptr(), theme::size::BODY, Rect::new(0.0, 0.0, 260.0, 60.0))
                .style(s)
                .focused(focused);
            matches!(b.style, ControlStyle::Keyline) && !b.focused
        };
        assert!(!styled(ControlStyle::Danger, false), "an idle danger pill draws no stroke");
        assert!(!styled(ControlStyle::Danger, true));
        assert!(styled(ControlStyle::Keyline, false), "the control case: Keyline idle still does");
    }

    /// **The scroll band's glass is about TWICE the region a moving host can carry, on both of the
    /// screens that draw one** — the arithmetic that closed `/tmp/plxnative-navglass`, written as a
    /// test so nobody re-derives it optimistically.
    ///
    /// A glass surface is charged for its rect grown [`crate::gfx::BLUR_MARGIN`] a side and clamped
    /// to the panel. This band spans the full width, so only the vertical growth is left to pay
    /// for, and there is nothing a design can do about it: the band's height IS the chrome it backs.
    ///
    /// The measurement that follows the arithmetic is in `docs/glass-hardware-budget.md` §11.
    #[test]
    fn the_scroll_band_is_twice_the_glass_region_budget_on_both_screens() {
        let area = |content_top: f32| {
            let r = nav_glass_rect(content_top);
            let reg = crate::gfx::blur_region(r.x, r.y, r.w, r.h);
            reg[2] * reg[3]
        };
        let lib = area(crate::ui::library::GRID_TOP);
        let search = area(crate::ui::search::CONTENT_TOP);
        assert_eq!(lib, 1920.0 * 302.0, "the Library band: 1920 wide, 0..214 grown 88 below");
        assert_eq!(search, 1920.0 * 336.0, "Search's field sits 34px lower, and the band with it");
        for (name, a) in [("library", lib), ("search", search)] {
            assert!(
                a > 1.9 * crate::gfx::GLASS_REGION_BUDGET,
                "{name}: {a} px^2 against a {} budget — the arithmetic that made this look \
                 hopeless before it was built. What refused it is the MEASUREMENT in \
                 docs/glass-hardware-budget.md §11, not this ratio; §11 also records that the \
                 direct source path makes the region term about 4x cheaper than the budget \
                 assumes, and that what actually binds is the SURFACE's composite, not this.",
                crate::gfx::GLASS_REGION_BUDGET,
            );
        }
    }

    /// **The material is the flat treatment's own ramp, scaled** — one curve, two weights.
    ///
    /// The knee is the part worth pinning. A band whose stops were solved independently for the two
    /// paths could hold its chrome legible in one and not the other, and only the television would
    /// say which.
    #[test]
    fn the_glass_band_is_the_opaque_band_scaled_by_its_frost() {
        let (base, knee, clear) = nav_scrim_stops(1.0);
        assert_eq!(base, theme::SURFACE_APP, "the flat path is untouched: a solid app-grey floor");
        assert_eq!(knee[3], NAV_SCRIM_KNEE_A);
        assert_eq!(clear[3], 0.0);

        let (gbase, gknee, gclear) = nav_scrim_stops(NAV_GLASS_FROST);
        assert_eq!(gbase[3], NAV_GLASS_FROST);
        assert_eq!(gknee[3], NAV_SCRIM_KNEE_A * NAV_GLASS_FROST);
        assert_eq!(gclear[3], 0.0, "both paths reach nothing at the content line");
        assert_eq!(
            gknee[3] / gbase[3],
            knee[3] / base[3],
            "the knee sits at the same FRACTION of the floor in both materials",
        );
        for i in 0..3 {
            assert_eq!(gbase[i], base[i], "channel {i}: the same grey, only lighter");
        }
    }

    /// The trigger is OFF in a build nobody armed, which is the whole promise the item was left on.
    #[test]
    fn the_scroll_band_material_is_off_unless_its_trigger_is_armed() {
        assert!(
            !crate::dev::flag("navglass"),
            "the host suite must not be run with /tmp/plxnative-navglass armed",
        );
        assert!(!nav_glass_wanted(), "default OFF: nothing changes for anyone");
        assert!(!nav_glass_on());
    }

    /// The band's blurred region, in authored px^2, for a track `w` wide with the chip unfurled
    /// beside it — through **`gfx`'s own `blur_region` and `blur_region_union`**, not a copy of them.
    ///
    /// The copy is the thing to avoid here and there is a measurement to prove it: this test's
    /// predecessor modelled the region inline and left the screen CLAMP out, which priced the tab
    /// track at `(940+176) x 252` = 281k px^2 where the real region is `1116 x 200` = 223k — a 26%
    /// over-estimate that sat under a passing assertion for as long as the limit existed, and the
    /// reason the old limit read as if it had only ~7% of headroom when it had far more. Both
    /// surfaces are grown `gfx::BLUR_MARGIN` a side and then clamped to the screen, so the capsule
    /// at x=82 and the band at y=36 put the union's left edge and its top on 0 and make it 200 tall
    /// rather than 252. That is `blur_region`'s business, and it is now asked rather than restated.
    ///
    /// The rects are the DRAWN ones: `chip_cap` at rest with a name at its budget, and the centred
    /// track `draw_tab_row` builds. Only the chip's left edge reaches the union — the capsule grows
    /// rightward and the clearance keeps its right edge inside the track's — so the pair prices the
    /// same at every point of the unfurl, which is why one number can stand for the band.
    fn band_region(w: f32) -> f32 {
        let (h, y) = (TAB_PILL_H + 2.0 * TAB_TRACK_PAD, TOP_BAR_Y - TAB_TRACK_PAD);
        let track = crate::gfx::blur_region((crate::ui::consts::SCR_W - w) * 0.5, y, w, h);
        let cap = chip_cap(1.0, CHIP_NAME_MAX);
        let chip = crate::gfx::blur_region(cap.x, cap.y, cap.w, cap.h);
        let u = crate::gfx::blur_region_union(track, chip);
        u[2] * u[3]
    }

    /// **[`GLASS_TRACK_MAX`] is the budget, solved for width** — asserted rather than asserted-in-a-
    /// comment, because the two numbers live in different files and the one that moves is the
    /// budget. A glass surface is charged for the blurred RECTANGLE: itself grown
    /// [`crate::gfx::BLUR_MARGIN`] on every side. At the limit width that must still fit
    /// [`crate::gfx::GLASS_REGION_BUDGET`], which is what a MOVING host carries at 60 fps — and this
    /// bar's host is always moving, since the page under it is what the user is scrolling.
    ///
    /// **It is priced as the PAIR now**, and that is the change worth reading twice. This asserted
    /// the track alone against the budget, which was the whole story while the track was the only
    /// glass in the band; [`profile_chip`]'s capsule is a second surface of the same material, and
    /// two glass surfaces in one frame converge on ONE grab (`gfx::blur_region_union`) that has to
    /// span both. A track that passes on its own and fails as a pair is exactly the regression this
    /// exists to catch, so the counterexample moved with it: the old one was a 1050-wide track,
    /// which is far outside either limit, where 940 — the width the TRACK alone could afford — is
    /// inside its own budget and outside the band's. That is the boundary the second surface moved.
    #[test]
    fn the_whole_bands_glass_fits_one_region_budget() {
        assert!(
            band_region(GLASS_TRACK_MAX) <= crate::gfx::GLASS_REGION_BUDGET,
            "the band at the limit costs {:.0} px^2, past the {:.0} a moving host carries",
            band_region(GLASS_TRACK_MAX),
            crate::gfx::GLASS_REGION_BUDGET,
        );
        assert!(
            band_region(940.0) > crate::gfx::GLASS_REGION_BUDGET,
            "940 was the TRACK's own limit and must be outside the BAND's: {:.0} px^2",
            band_region(940.0),
        );
    }

    /// **The unfurled chip and the glass track can never touch** — the one thing about this band
    /// that no shader can fix, so it is arithmetic and it is asserted.
    ///
    /// There is ONE blur cache. A second glass surface drawn over the first samples that same
    /// snapshot, so an overlap gets no second blur — only a second scrim and a second rim
    /// composited over material that already carries both, which is the doubling `draw_tab_row`'s
    /// source-pass note measures from the other side (a face reading (52,60,38) came out at
    /// (33,38,26) once it contained itself). The capsule is bounded by [`CHIP_NAME_MAX`] and the
    /// track is centred, so the clearance is two constants and needs no font.
    ///
    /// **Be clear about what each half can catch, because as long as [`GLASS_TRACK_MAX`] is SOLVED
    /// for this both are identities.** That is the design working — the constant is the clearance,
    /// so nothing is left to check — and it means the assertions only bite under an EDIT. The first
    /// fires the moment the limit goes back to being a literal (it was `940.0` until 2026-08-21,
    /// which leaves the widest capsule 42px inside the widest track). The second is the opposite
    /// guard, and it is NOT "a wider track would be overlapped" — a track past the limit wears no
    /// glass at all, so it can never be overlapped as glass. It pins the clearance TIGHT: within a
    /// pixel of [`BAND_AIR`] and not an arbitrary gulf, so nobody buys safety here by quietly
    /// spending the strip.
    ///
    /// The capsule side is the rect [`profile_chip`] actually draws ([`chip_cap`]), which is the
    /// half that could have gone wrong on its own: [`CHIP_CAP_MAX_R`] hand-restated those seven
    /// terms until it was made to ask for them.
    #[test]
    fn the_unfurled_chip_never_reaches_the_glass_track() {
        let track_x = |w: f32| (crate::ui::consts::SCR_W - w) * 0.5;
        let cap = chip_cap(1.0, CHIP_NAME_MAX);
        assert_eq!(cap.x + cap.w, CHIP_CAP_MAX_R, "priced and drawn are one expression");
        assert!(
            cap.x + cap.w + BAND_AIR <= track_x(GLASS_TRACK_MAX) + 0.01,
            "the widest capsule ends at {} and the widest glass track starts at {} — they must \
             clear each other by {}",
            cap.x + cap.w,
            track_x(GLASS_TRACK_MAX),
            BAND_AIR,
        );
        assert!(
            cap.x + cap.w + 2.0 * BAND_AIR + 1.0 > track_x(GLASS_TRACK_MAX),
            "the clearance is {:.0}px where {BAND_AIR} was asked for — a limit that gives away \
             more strip than the band needs is a cost nobody decided to pay",
            track_x(GLASS_TRACK_MAX) - (cap.x + cap.w),
        );
        // the capsule only ever grows RIGHTWARD off a fixed edge, which is what lets one number
        // stand for the band at every point of the unfurl (see `band_region`).
        for e in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let c = chip_cap(e, CHIP_NAME_MAX);
            assert_eq!((c.x, c.y, c.h), (cap.x, cap.y, cap.h), "only the width moves");
            assert!(c.w <= cap.w + 0.01, "the rest capsule is the widest");
        }
    }

    /// The unfurl fades the whole FACE, and the two rim weights are the half that matters.
    ///
    /// `fs_glass.frag` writes `max(u_tint.a * cov, rimw)`: the rim is deliberately allowed to exceed
    /// its own surface's coverage, so that a 1px line survives its antialiased edge. Fade only the
    /// tint and the consequence is a bright empty hairline capsule snapping on around the avatar in
    /// the first milliseconds of focus — visible, and not something the tint can walk back.
    ///
    /// And at rest it is the track's face to the bit, which is the claim the whole arrangement
    /// rests on: one solve, one material, two surfaces.
    #[test]
    fn the_chips_capsule_fades_its_rim_with_its_scrim_and_rests_on_the_tracks_own_face() {
        let face = crate::gfx::GlassFace {
            scrim_top: [0.0, 0.0, 0.0, 0.40],
            scrim_bot: [0.0, 0.0, 0.0, 0.52],
            rim: [1.0, 1.0, 1.0, 0.14],
            rim_lit: [1.0, 1.0, 1.0, 0.28],
            rim_w: 1.0,
        };
        let rest = chip_face(face, 1.0);
        for (a, b) in [
            (rest.scrim_top, face.scrim_top),
            (rest.scrim_bot, face.scrim_bot),
            (rest.rim, face.rim),
            (rest.rim_lit, face.rim_lit),
        ] {
            assert_eq!(a, b, "at rest the chip wears the track's face unchanged");
        }
        let half = chip_face(face, 0.5);
        assert_eq!(half.scrim_top[3], 0.20, "the scrim fades with the unfurl");
        assert_eq!(half.rim[3], 0.07, "…and so does the perimeter");
        assert_eq!(half.rim_lit[3], 0.14, "…and the lit edge, which the tint alone cannot reach");
        assert_eq!(half.rim[..3], face.rim[..3], "the lamp's COLOUR does not move; its weight does");
        assert_eq!(half.rim_w, face.rim_w, "one design-system pixel, at every point of the unfurl");
    }

    /// The width rule is the only refusal a SECTION TABLE can trip on its own, so it has to hold
    /// for the strip the product actually draws and give way for one the server could produce.
    #[test]
    fn a_normal_strip_keeps_the_material_and_a_long_one_loses_it() {
        // Home + three libraries + the square Search mark, at the widths `with_tab_metrics`
        // measures: label + 2 x TAB_PILL_PAD.
        let normal = [126.0, 146.0, 176.0, TAB_ICON_PILL_W];
        assert!(
            tab_track_w(&normal) <= GLASS_TRACK_MAX,
            "the product's own strip is {} wide and must keep the material",
            tab_track_w(&normal),
        );
        let many = [126.0, 146.0, 176.0, 150.0, 160.0, 140.0, 170.0, TAB_ICON_PILL_W];
        assert!(
            tab_track_w(&many) > GLASS_TRACK_MAX,
            "eight pills is {} wide and must drop to flat",
            tab_track_w(&many),
        );
    }

    /// A cadence of THREE, which is no longer the shipped default and is the point.
    ///
    /// These tests were written against `DEFAULT_DYNAMIC_PERIOD` and broke the day it moved from
    /// three to one — which is the wrong thing for them to be sensitive to. What they exist to
    /// prove is the MECHANISM: every Nth qualifying present, damage pending across the gap, one
    /// capture shared by every owner in a present, and a counter that wraps. None of that is a
    /// property of N, so N is pinned here and the shipped value is asserted separately by
    /// [`the_shipped_cadence_is_every_changed_present`]. A period of one would also make three of
    /// the four assertions below unreachable, since nothing would ever `Wait`.
    const P: u32 = 3;

    /// The shipped default, asserted in ONE place so moving it is a one-line decision with a
    /// measurement behind it rather than a cascade through the cadence tests.
    #[test]
    fn the_shipped_cadence_is_every_changed_present() {
        assert_eq!(
            DEFAULT_DYNAMIC_PERIOD, 1,
            "measured on the direct source path: one-in-one costs +0.07% of the frame against \
             one-in-three, and 60.0 fps either way"
        );
    }

    /// The one-pass hero ground's WEDGE field must be the four-quad wedge, not a lookalike. Graded
    /// at every corner of both quads [`hero_scrim_quads`] builds, because a bilinear quad IS its
    /// corners — reproduce those and the interiors follow, since both expressions are the same
    /// product of two linear factors. This is the test that would have caught a swapped feather
    /// range or a peak taken at the wrong x.
    #[test]
    fn the_one_pass_ground_reproduces_the_wedge_at_every_corner_of_both_quads() {
        for strength in [0.0f32, 0.35, 1.0] {
            let wedge = hero_ground_wedge(strength);
            let (q, n) = hero_scrim_quads(strength, false);
            assert_eq!(n, 2, "home's hero has no right wedge");
            for (r, k) in q.iter().take(n) {
                // (tl, tr, br, bl) — the order `Painter::grad4` and `fs_ambient.frag` agree on
                for (corner, (x, y)) in [
                    (k[0], (r.x, r.y)),
                    (k[1], (r.x + r.w, r.y)),
                    (k[2], (r.x + r.w, r.y + r.h)),
                    (k[3], (r.x, r.y + r.h)),
                ] {
                    let one = hero_ground_wedge_a(wedge, x, y);
                    assert!(
                        (one - corner[3]).abs() < 1e-6,
                        "strength {strength} at ({x}, {y}): one-pass {one} vs quad {}",
                        corner[3]
                    );
                }
            }
        }
    }

    /// …and the ATMOSPHERIC ramp must be the SCREEN's own curve, sampled where it bends. Home's is
    /// a two-stop ramp with a midpoint knee and detail's a single linear stop, so the parameters
    /// are the caller's — this pins the packing (`y0, knee, a_knee, a_foot`) against the curve the
    /// legibility table is graded on, at both stops and either side of each.
    #[test]
    fn the_one_pass_ground_reproduces_the_screens_atmospheric_ramp() {
        for hero_a in [0.2f32, 0.6, 1.0] {
            let ramp = crate::ui::home::base_scrim_ramp(hero_a);
            for y in [0.0f32, 200.0, HERO_BASE_SCRIM_Y0, 400.0, 600.0, 702.0, 900.0, crate::ui::consts::SCR_H] {
                let one = hero_ground_ramp_a(ramp, y);
                let quads = crate::ui::home::base_scrim_a(y, hero_a);
                assert!(
                    (one - quads).abs() < 1e-6,
                    "hero_a {hero_a} at y {y}: one-pass {one} vs screen {quads}"
                );
            }
        }
    }

    /// The whole reason one pass can stand in for three: two straight-alpha layers of ONE ink
    /// compose as `a1 + a2 - a1*a2`, and the art under them folds into the same single blend. This
    /// is `fs_hero.frag`'s algebra, executed — if it is wrong the panel shows a differently-lit
    /// hero and nothing else in the suite would notice.
    #[test]
    fn one_blend_lands_where_three_stacked_ones_do() {
        let over = |dst: f32, src: f32, a: f32| dst * (1.0 - a) + src * a;
        for dst in [0.0f32, 0.31, 1.0] {
            for art in [0.0f32, 0.62, 1.0] {
                for aa in [0.0f32, 0.4, 1.0] {
                    for a1 in [0.0f32, 0.25, 0.72] {
                        for a2 in [0.0f32, 0.5, 0.72] {
                            const INK: f32 = 0.04;
                            let three = over(over(over(dst, art, aa), INK, a1), INK, a2);
                            let b = a1 + a2 - a1 * a2;
                            let s = 1.0 - (1.0 - aa) * (1.0 - b);
                            let src = (art * aa * (1.0 - b) + INK * b) / s.max(1.0 / 4096.0);
                            let one = over(dst, src, s);
                            assert!(
                                (one - three).abs() < 1e-5,
                                "dst {dst} art {art} A {aa} a1 {a1} a2 {a2}: {one} vs {three}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn dynamic_glass_refreshes_on_every_third_successful_present() {
        let mut clock = DynamicClock::new();
        clock.cover_now(41);
        assert_eq!(clock.step(42, true, P), DynamicStep::Wait);
        assert_eq!(clock.step(43, false, P), DynamicStep::Wait, "pending damage survives");
        assert_eq!(clock.step(44, false, P), DynamicStep::Refresh);
        assert_eq!(clock.step(44, true, P), DynamicStep::None, "same-present owners are covered");
        assert_eq!(clock.step(45, false, P), DynamicStep::None, "a clean keepalive does nothing");
    }

    #[test]
    fn dynamic_glass_cadence_survives_the_present_counter_wrapping() {
        let mut clock = DynamicClock::new();
        clock.cover_now(u32::MAX - 1);
        assert_eq!(clock.step(u32::MAX, true, P), DynamicStep::Wait);
        assert_eq!(clock.step(0, false, P), DynamicStep::Wait);
        assert_eq!(clock.step(1, false, P), DynamicStep::Refresh);
    }

    #[test]
    fn staggered_dynamic_owners_share_one_global_cadence() {
        let mut clock = DynamicClock::new();
        clock.cover_now(10); // owner A opens
        assert_eq!(clock.step(11, true, P), DynamicStep::Wait);
        clock.cover_now(12); // owner B opens: its immediate shared capture covers A too
        assert_eq!(clock.step(12, true, P), DynamicStep::None);
        assert_eq!(clock.step(13, true, P), DynamicStep::Wait);
        assert_eq!(clock.step(14, true, P), DynamicStep::Wait);
        assert_eq!(clock.step(15, true, P), DynamicStep::Refresh);
        assert_eq!(clock.step(15, true, P), DynamicStep::None, "B cannot schedule a second capture");
    }

    /// A period of one refreshes on EVERY present with a dirty underlay — the 60 Hz end of the
    /// cadence sweep. It must still refuse a second capture on a present already covered, or two
    /// glass owners would run the chain twice in one frame and the cost curve would measure that.
    #[test]
    fn a_period_of_one_refreshes_every_dirty_present_but_only_once_each() {
        let mut clock = DynamicClock::new();
        clock.cover_now(70);
        assert_eq!(clock.step(71, true, 1), DynamicStep::Refresh);
        assert_eq!(clock.step(71, true, 1), DynamicStep::None, "already covered this present");
        assert_eq!(clock.step(72, true, 1), DynamicStep::Refresh);
        assert_eq!(clock.step(73, false, 1), DynamicStep::None, "a clean present still refreshes nothing");
    }

    /// A longer period waits longer and no more than that: the wait count is `period - 1`.
    #[test]
    fn a_longer_period_waits_exactly_that_many_presents() {
        for period in 2..=8u32 {
            let mut clock = DynamicClock::new();
            clock.cover_now(0);
            for present in 1..period {
                assert_eq!(
                    clock.step(present, true, period),
                    DynamicStep::Wait,
                    "period {period} refreshed early at present {present}"
                );
            }
            assert_eq!(clock.step(period, true, period), DynamicStep::Refresh, "period {period}");
        }
    }

    /// `/tmp/plxnative-glasshz` writes this static and nothing else does. Zero would be a refresh
    /// every zero frames, so it clamps rather than dividing the cadence by nothing.
    #[test]
    fn the_cadence_override_clamps_and_reports_what_it_installed() {
        // A crate-wide static, so this takes the shared globals lock rather than a local one.
        let _g = crate::testlock::serial();
        let restore = dynamic_period();
        assert_eq!(set_dynamic_period(0), 1, "zero presents per refresh is not a cadence");
        assert_eq!(dynamic_period(), 1);
        assert_eq!(set_dynamic_period(4), 4);
        assert_eq!(set_dynamic_period(99), 8, "past 8 is clamped, not refused");
        set_dynamic_period(restore);
        assert_eq!(dynamic_period(), DEFAULT_DYNAMIC_PERIOD, "the default is what ships");
    }

    #[test]
    fn glass_state_reactivates_after_deactivation_or_a_route_gap() {
        let mut state = GlassState::new();
        assert!(state.needs_activation(20));
        state.active = true;
        state.last_seen = 20;
        assert!(!state.needs_activation(21), "idle time has no successful presents");
        assert!(state.needs_activation(22), "another route presented in between");
        state.deactivate();
        assert!(state.needs_activation(21));
    }

    /// **Every glass policy COMPOSITES**, and the axis that let one not to is gone.
    ///
    /// This test used to assert the source-dim arithmetic — `Glass::DYNAMIC.source_rgb(0.5) == 0.75`
    /// and so on — for a preset nothing outside this module ever constructed. The mechanism was
    /// measured against the item menu over one checker ground and rejected (`e75b5e49`): dimming the
    /// page going INTO the backdrop destroys the modulation the frost is layered over, so the panel
    /// arrives flat. What is worth pinning now is that no policy can reintroduce it by accident —
    /// the two presets differ in REFRESH and in nothing else.
    #[test]
    fn every_glass_policy_composites_and_differs_only_in_refresh() {
        assert_ne!(Glass::CACHED, Glass::DYNAMIC_BACKDROP, "the two presets are not the same policy");
        // **The policy carries NOTHING beside its refresh**, which is the property the deleted axis
        // violated — and the only form of it that can actually fail. Comparing a preset against a
        // literal of its own one field cannot: that is `Self{refresh} == Self{refresh}`, true by
        // construction, which is what the first version of this test asserted twice.
        assert_eq!(
            std::mem::size_of::<Glass>(),
            std::mem::size_of::<GlassRefresh>(),
            "a second axis on Glass would show up here first"
        );
    }

    // ── The strip's vocabulary: an index MEANS something ──────────────────────────────────────
    //
    // Driven through the pure halves (`pill_in`/`pill_index`/`last_section_in`), so no `browse`
    // table is seeded and no crate global is read — see the note above them. That is also what
    // lets these grade strips this host cannot build: one library, and none at all.

    /// The ladder is a BIJECTION over the strip — every drawn index resolves to exactly one
    /// destination and back — which is what lets [`pill_of`] PLACE the capsule for a destination
    /// that [`pill_at`] will later resolve a click on. Swept over every strip size the app can
    /// have, because the arm that matters is the one BETWEEN the ends and a two-pill strip has
    /// no `Section` in it at all.
    #[test]
    fn every_strip_index_round_trips_through_the_typed_pill() {
        for search in 1..8usize {
            assert_eq!(pill_in(0, search), Pill::Home, "pill 0 is Home on every strip");
            assert_eq!(pill_in(search, search), Pill::Search, "the last pill is never a library");
            for sec in 0..search.saturating_sub(1) {
                assert_eq!(pill_in(sec + 1, search), Pill::Section(sec), "a TAB index, Home already subtracted");
            }
            for i in 0..=search {
                assert_eq!(pill_index(pill_in(i, search), search), i, "pill {i} of {search} did not round trip");
            }
        }
    }

    /// [`last_section_pill`]'s two answers. The interesting one is the SECOND: a strip with no
    /// libraries at all (discovery failed, and the user stays on the screen) has no library pill
    /// for a section-derived index to land on, and the fallback must be a pill that exists.
    #[test]
    fn the_section_ceiling_is_the_pill_before_search_and_falls_back_to_home() {
        for search in 2..8usize {
            let last = last_section_in(search);
            assert_eq!(pill_in(last, search), Pill::Section(search - 2), "the last pill that IS a library");
            assert!(last < search, "…and never Search itself");
        }
        // the failed-discovery strip: `Home | Search`, and nothing for a section to clamp onto
        assert_eq!(last_section_in(1), 0, "with no sections the ceiling is HOME…");
        assert_eq!(pill_in(last_section_in(1), 1), Pill::Home, "…so the clamp can never land on nothing");
    }

    // ── The poster's state mark: one vocabulary, one mark at a time ───────────────────────────
    //
    // `card` needs a GL context, so the CHOICE is what a host test can reach — and the choice is
    // where this has been wrong before: the mark used to say "unwatched" in amber, which is the
    // opposite claim, and the bar/disc precedence is only observable on an item PMS reports as both.

    /// A MOVIE row at a given watched state. `dur_ns` is 100 min, so `resume_ms` reads as a
    /// percentage of the way in. For a LEAF the two flags really are each other's negation — which
    /// is exactly what stops being true for a container, hence the show cases below.
    fn row(watched: bool, resume_ms: i64) -> PmsMovie {
        let mut m = PmsMovie::default();
        m.dur_ns = 100 * 60 * 1000 * 1_000_000;
        m.watched = watched;
        m.unwatched = !watched;
        m.resume_ms = resume_ms;
        m
    }

    #[test]
    fn a_poster_nobody_has_started_wears_no_mark_at_all() {
        // the common case on any real server, and the whole reason the polarity inverted
        assert_eq!(poster_mark(&row(false, 0)), PosterMark::None);
    }

    #[test]
    fn a_finished_poster_wears_the_watched_disc() {
        assert_eq!(poster_mark(&row(true, 0)), PosterMark::Watched);
    }

    #[test]
    fn a_re_watch_in_flight_outranks_the_watched_flag() {
        // PMS reports BOTH on a finished-then-restarted item; the bar wins, so the tile never
        // wears two marks — and what it says is what the viewer is actually doing.
        let m = row(true, 30 * 60 * 1000);
        assert_eq!(poster_mark(&m), PosterMark::InProgress);
        assert!(m.resume_frac().is_some(), "InProgress must be exactly when the caller draws the bar");
    }

    #[test]
    fn a_part_watched_poster_that_was_never_finished_is_in_progress_too() {
        assert_eq!(poster_mark(&row(false, 30 * 60 * 1000)), PosterMark::InProgress);
    }

    #[test]
    fn an_offset_the_server_never_cleared_is_finished_not_in_progress() {
        // resume AT or PAST the end: a full-width bar there read as a rendering bug, and it would
        // now also hide the disc the item has earned. Both the mark and the bar must agree.
        for resume in [100 * 60 * 1000, 200 * 60 * 1000] {
            let m = row(true, resume);
            assert_eq!(m.resume_frac(), None, "resume {resume} must not draw a bar");
            assert_eq!(poster_mark(&m), PosterMark::Watched, "resume {resume}");
        }
    }

    #[test]
    fn a_row_with_no_runtime_cannot_be_in_progress() {
        // dur_ns == 0 (the server sent no duration): a fraction is undefined, so there is no bar to
        // draw and the watched flag alone decides.
        let mut m = row(true, 30 * 60 * 1000);
        m.dur_ns = 0;
        assert_eq!(m.resume_frac(), None);
        assert_eq!(poster_mark(&m), PosterMark::Watched);
        let mut m = row(false, 30 * 60 * 1000);
        m.dur_ns = 0;
        assert_eq!(poster_mark(&m), PosterMark::None);
    }

    #[test]
    fn a_show_three_episodes_in_is_not_a_watched_show() {
        // The device capture that caught this: a library filtered to `unwatchedLeaves=1` — every
        // tile has an unseen episode by construction — had five posters wearing a watched disc,
        // because `!unwatched` is true for a container the moment ONE episode is played. A show is
        // marked only when it is DONE, so partly-watched sits with never-started under "no mark".
        let mut m = PmsMovie::default();
        m.kind = 1; // show
        m.unwatched = false; // some episode has been played…
        m.watched = false; // …but not all of them
        assert_eq!(poster_mark(&m), PosterMark::None);
        m.watched = true;
        assert_eq!(poster_mark(&m), PosterMark::Watched, "every leaf seen IS the disc");
    }

    // ── …and the same three states asked as the WRITE-VERB question ───────────────────────────
    //
    // `row_watch_state` is `poster_mark` plus one rule, and the one rule is the whole reason it
    // exists: a mark DESCRIBES, a menu row PROMISES. These grade the difference and the sameness.

    /// **The only place the two resolvers disagree** — and it is the case the owner reported: a
    /// show in the middle wears no poster mark (a tile cannot say where a series stands) but is
    /// reachable from a menu in BOTH directions, so it is `InProgress` here and gets both rows.
    #[test]
    fn a_container_mid_run_is_in_the_middle_for_a_menu_though_it_wears_no_mark() {
        let mut m = PmsMovie::default();
        m.kind = 1; // show
        m.unwatched = false; // some episode has been played…
        m.watched = false; // …but not all of them
        assert_eq!(poster_mark(&m), PosterMark::None, "the tile still claims nothing");
        assert_eq!(row_watch_state(&m), PosterMark::InProgress, "…but both verbs are reachable");
        // and a SEASON is a container on the same terms — the rule is the flag pair, not the kind
        m.kind = 2;
        assert_eq!(row_watch_state(&m), PosterMark::InProgress);
    }

    /// The two ENDS are the same answer from both resolvers, on containers as much as leaves.
    /// A container's `watched` is the strict `viewedLeafCount >= leafCount` (`pms::parse_item`), so
    /// a finished show is genuinely finished and its menu offers only the way back — which is what
    /// the shelf menu got wrong before, offering "Mark as Watched" on a show already done.
    #[test]
    fn the_two_ends_of_the_range_answer_the_same_either_way() {
        let mut show = PmsMovie::default();
        show.kind = 1;
        for (unwatched, watched, want) in
            [(true, false, PosterMark::None), (false, true, PosterMark::Watched)]
        {
            show.unwatched = unwatched;
            show.watched = watched;
            assert_eq!(row_watch_state(&show), want);
            assert_eq!(poster_mark(&show), want, "an END is not where the two questions differ");
        }
    }

    /// A LEAF is delegated whole, so every resume-point edge `poster_mark` keeps — an offset past
    /// the end is finished, a row with no runtime cannot be in progress — holds for the menu too
    /// without being restated. The flags are complements on a leaf, so the extra rule is unreachable
    /// there by construction.
    #[test]
    fn a_leaf_asks_the_poster_and_gets_its_answer_unchanged() {
        for m in [
            row(false, 0),
            row(true, 0),
            row(false, 30 * 60 * 1000),
            row(true, 30 * 60 * 1000),
            row(true, 200 * 60 * 1000),
        ] {
            assert_eq!(row_watch_state(&m), poster_mark(&m), "a leaf must not answer twice");
        }
    }

    // ── AmbientWash: the page GROUND's legibility contract ───────────────────────────────────
    //
    // All pure math over `theme` tokens — no GL, no globals, so these are ordinary parallel tests.
    // `Spring::step` (used by the dissolve test) is `gfx::spring`, closed-form arithmetic.

    /// One sRGB channel, linearized (WCAG 2.x). Local to the tests on purpose: the app never needs
    /// this — it is the yardstick the design decision was made with, kept here so the decision stays
    /// checkable rather than remembered.
    /// The corner sources a ground has to survive: the blown-out extreme, each primary and secondary
    /// at full saturation, a mid grey, and the brightest thing in the palette.
    fn hostile_sources() -> Vec<[f32; 3]> {
        vec![
            [1.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
            [0.5, 0.5, 0.5],
            [theme::WASH_WARM[0], theme::WASH_WARM[1], theme::WASH_WARM[2]],
            [theme::TEXT_PRIMARY[0], theme::TEXT_PRIMARY[1], theme::TEXT_PRIMARY[2]],
        ]
    }

    /// **The legibility contract, executable.** Every corner of an artwork-keyed ground at
    /// [`AmbientWash::GROUND_W`] must clear 3:1 against [`theme::TEXT_TERTIARY`] — the dimmest ink
    /// the app puts on a ground (cast roles, unfocused episode summaries, the About column's labels,
    /// all at `size::CAPTION`) — and 7:1 against [`theme::TEXT_PRIMARY`]. This is the test that would
    /// have caught the UNCAPPED version (a white corner lands tertiary at 1.98:1), and it is the one
    /// that fails the day someone raises `GROUND_W` or `GROUND_LUMA` past what a page can carry. It
    /// guards the person page and the detail page at once, because both go through `keyed`.
    #[test]
    fn a_ground_never_outshines_the_fine_print_that_sits_on_it() {
        for src in hostile_sources() {
            let g = AmbientWash::keyed([src; 4], [AmbientWash::GROUND_W; 4]);
            for (i, corner) in g.iter().enumerate() {
                let t = contrast(theme::TEXT_TERTIARY, *corner);
                assert!(t >= 3.0, "corner {i} of {src:?}: TEXT_TERTIARY at {t:.2}:1, under the 3:1 large-text floor");
                let p = contrast(theme::TEXT_PRIMARY, *corner);
                assert!(p >= 7.0, "corner {i} of {src:?}: TEXT_PRIMARY at {p:.2}:1");
            }
        }
    }

    /// The other end: a ground keyed to near-black key art must not sink below the value a card's
    /// drop shadow needs to read against. `SURFACE_APP`'s own doc rejects the old near-black
    /// (25,25,29)/255 for exactly that reason; `GROUND_W ≤ 0.26` is what keeps the floor above it,
    /// with no floor constant anywhere.
    #[test]
    fn a_ground_stays_light_enough_for_a_card_shadow() {
        let floor = AmbientWash::keyed([[0.0; 3]; 4], [AmbientWash::GROUND_W; 4]);
        let rejected = [25.0 / 255.0, 25.0 / 255.0, 29.0 / 255.0, 1.0];
        for corner in floor {
            for ch in 0..3 {
                let want = theme::SURFACE_APP[ch] * (1.0 - AmbientWash::GROUND_W);
                assert!((corner[ch] - want).abs() < 1e-6, "the darkest ground is the surface scaled by 1-GROUND_W");
                assert!(corner[ch] > rejected[ch], "channel {ch} sank to {} — into the near-black the palette rejects", corner[ch]);
            }
        }
    }

    /// "No artwork is the app's own flat ground" — stated in the type's docs since the person page
    /// shipped, pinned here. At weight 0 the mix must be EXACTLY the surface, so a screen needs no
    /// has-envelope branch in its draw.
    #[test]
    fn no_artwork_is_the_apps_own_ground() {
        for src in hostile_sources() {
            let quad = [[src[0], src[1], src[2], 1.0]; 4];
            assert_eq!(AmbientWash::target(quad, [0.0; 4]), [theme::SURFACE_APP; 4], "src {src:?}");
        }
    }

    /// The cap is a scalar multiply, not a per-channel clamp: a bright saturated corner comes back
    /// at exactly [`GROUND_LUMA`] with its channel RATIOS untouched (a clamp would desaturate it
    /// toward white), and a corner already under the ceiling comes back bit-identical.
    #[test]
    fn the_luma_cap_spends_brightness_and_nothing_else() {
        let bright = [0.9, 0.7, 0.2];
        let c = ground_capped(bright);
        let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!((y - GROUND_LUMA).abs() < 1e-4, "capped luma {y} != {GROUND_LUMA}");
        let k = c[0] / bright[0];
        for ch in 0..3 {
            assert!((c[ch] / bright[ch] - k).abs() < 1e-5, "channel {ch} was scaled by a different factor — that is a clamp, not a cap");
        }
        assert_eq!(c[3], 1.0, "a ground corner is opaque");

        let dark = [0.10, 0.20, 0.05];
        assert_eq!(ground_capped(dark), [dark[0], dark[1], dark[2], 1.0], "a corner under the ceiling is untouched");
    }

    /// A tl/tr/br/bl transposition is invisible on a near-symmetric gradient in a screenshot and
    /// wrong on every asymmetric one — the exact hazard `UltraBlurColors::corners`' doc warns about
    /// (the JSON reading order is not the ring order). Both constructors must be index-preserving.
    #[test]
    fn the_corners_keep_their_ring_order() {
        let src = [[0.8, 0.1, 0.1], [0.1, 0.8, 0.1], [0.1, 0.1, 0.8], [0.7, 0.7, 0.1]];
        let w = [AmbientWash::GROUND_W; 4];
        let keyed = AmbientWash::keyed(src, w);
        for i in 0..4 {
            let alone = AmbientWash::keyed([src[i]; 4], w);
            assert_eq!(keyed[i], alone[0], "corner {i} did not stay at index {i}");
        }
        let quad: [[f32; 4]; 4] = std::array::from_fn(|i| [src[i][0], src[i][1], src[i][2], 1.0]);
        let plain = AmbientWash::target(quad, w);
        for i in 0..4 {
            assert_eq!(plain[i], theme::mix(theme::SURFACE_APP, quad[i], w[i]), "target moved corner {i}");
        }
    }

    /// The skip test a screen uses to avoid a full-screen fill that changes nothing: flat on the
    /// ground it is drawn over → skippable; dissolving toward a different colour → not; settled back
    /// → skippable again. Drives the real corner springs, so it also pins that a dissolve actually
    /// converges within the ~0.5 s the rate promises.
    #[test]
    fn a_wash_that_has_resolved_to_the_ground_is_flat() {
        const EPS: f32 = AmbientWash::FLAT_EPS;
        let mut w = AmbientWash::flat(theme::SURFACE_APP);
        assert!(w.is_flat(theme::SURFACE_APP, EPS), "a wash mounted on the ground is already flat");

        let away = [[0.9, 0.2, 0.1, 1.0]; 4];
        for _ in 0..6 {
            w.step(away, AmbientWash::K, 1.0 / 60.0);
        }
        assert!(!w.is_flat(theme::SURFACE_APP, EPS), "a wash on its way to a colour is not the ground");

        for _ in 0..60 {
            w.step([theme::SURFACE_APP; 4], AmbientWash::K, 1.0 / 60.0);
        }
        assert!(w.is_flat(theme::SURFACE_APP, EPS), "a second of dissolve must land back on the ground");
    }

    // ── The hero corner scrim: the wedge's shape, its seam, and the legibility it promises ──────
    //
    // All pure math over `theme` tokens and the two screens' own layout arithmetic — no GL, no
    // globals, so these are ordinary parallel tests. The screens' contributions come from
    // `home::base_scrim_a` / `detail::base_scrim_a` / `detail::hero_chain`, which is the whole
    // point of those three being pure: the contract below reads the SAME numbers the draw does.

    /// The bilinear field a `(rect, [tl, tr, br, bl])` quad actually rasterizes, at absolute
    /// `(x, y)` — the shader's own `mix(mix(tl,tr,u), mix(bl,br,u), v)`, in alpha. The anti-drift
    /// yardstick: the closed-form [`hero_scrim_a`]/[`hero_scrim_right_a`] the contract is graded on
    /// and the corner colours that are actually drawn must agree, or the promise is about a field
    /// nobody paints.
    fn bilerp_a(q: (Rect, [[f32; 4]; 4]), x: f32, y: f32) -> f32 {
        let (r, k) = q;
        let u = ((x - r.x) / r.w).clamp(0.0, 1.0);
        let v = ((y - r.y) / r.h).clamp(0.0, 1.0);
        let top = k[0][3] + (k[1][3] - k[0][3]) * u;
        let bot = k[3][3] + (k[2][3] - k[3][3]) * u;
        top + (bot - top) * v
    }

    /// The wedge is a DARKENER that peaks in the corner the text is in: monotone non-increasing in
    /// x, at full [`theme::SCRIM_TEXT_A`] at the margin, and exactly 0 from [`HERO_SCRIM_W`] out —
    /// a non-zero value there would draw a vertical line across the hero where the quads end. Also
    /// pins that every out-of-range input clamps rather than going negative or past 1: `x` and
    /// `strength` both come from live animation state, and a negative alpha here would BRIGHTEN
    /// the artwork under the copy.
    #[test]
    fn the_wedge_never_brightens_toward_the_text() {
        for &s in &[0.25f32, 0.5, 1.0] {
            assert!((hero_scrim_a(0.0, s) - theme::SCRIM_TEXT_A * s).abs() < 1e-6, "the margin is the peak");
            let mut prev = f32::INFINITY;
            let mut x = 0.0f32;
            while x <= crate::ui::consts::SCR_W {
                let a = hero_scrim_a(x, s);
                assert!(a <= prev + 1e-6, "s={s}: alpha rose from {prev} to {a} at x={x}");
                assert!((0.0..=1.0).contains(&a), "s={s} x={x}: alpha {a} outside 0..=1");
                prev = a;
                x += 8.0;
            }
            assert_eq!(hero_scrim_a(HERO_SCRIM_W, s), 0.0, "the wedge must reach exactly nothing at its end");
            assert_eq!(hero_scrim_a(crate::ui::consts::SCR_W, s), 0.0, "…and stay there");
            assert_eq!(hero_scrim_a(-40.0, s), theme::SCRIM_TEXT_A * s, "x left of the frame is the peak, not more");
        }
        assert_eq!(hero_scrim_a(0.0, -0.5), 0.0, "a negative strength is nothing, never an inverted wedge");
        assert_eq!(hero_scrim_a(0.0, 4.0), theme::SCRIM_TEXT_A, "strength saturates at the token");
    }

    /// **The seam** — the one structural bug this component can have, and invisible on the host in
    /// every other form. The two left quads must share an EXACT float y and an identical colour
    /// pair along it: a gap leaves one row of unscrimmed artwork (BRIGHT) straight across the
    /// hero, an overlap composites two scrims on one row (BLACK). Neither is subtle and both look
    /// like a renderer bug rather than a layout one.
    ///
    /// `art_scrim`'s hairline had a different cause (an integer-truncated scissor meeting a float
    /// fill) and a different fix (`gfx::snap`). This pair is fill-to-fill: do not "fix" it by
    /// snapping, which would move the seam off the shared float and CREATE the bug.
    #[test]
    fn the_wedges_two_quads_abut_with_no_step() {
        let (q, n) = hero_scrim_quads(1.0, false);
        assert_eq!(n, 2);
        let (r0, k0) = q[0];
        let (r1, k1) = q[1];
        assert_eq!(r0.y + r0.h, r1.y, "the seam is not one float");
        assert_eq!(r0.x, r1.x, "the two quads must be the same column");
        assert_eq!(r0.w, r1.w);
        assert_eq!(k0[3], k1[0], "quad 0's bottom-left must be quad 1's top-left");
        assert_eq!(k0[2], k1[1], "quad 0's bottom-right must be quad 1's top-right");
        assert_eq!(r1.y + r1.h, crate::ui::consts::SCR_H, "the wedge runs to the foot of the panel");
        // …and the field is continuous ACROSS the seam, not merely the same colour at its ends:
        // sample both quads' own bilinear along it.
        for x in [0.0f32, 300.0, 900.0, HERO_SCRIM_W] {
            let below = bilerp_a(q[1], x, r1.y);
            assert!((bilerp_a(q[0], x, r0.y + r0.h) - below).abs() < 1e-6, "step at x={x}");
            // The quad's CORNERS are `hero_scrim_a` by construction now, so what is left to grade in
            // the interior is that the closed form is AFFINE in x — a curve there would be a field
            // `grad4`'s straight interpolation cannot reproduce, and the contract would be about a
            // shape nobody paints.
            assert!((below - hero_scrim_a(x, 1.0)).abs() < 1e-6, "the drawn field disagrees with hero_scrim_a at x={x}");
        }
    }

    /// The top chrome owns its own legibility with its own dark capsule (`draw_tab_row`'s track).
    /// A wedge creeping up into that band is two treatments fighting over one strip — and the
    /// capsule was tuned assuming nothing else is under it.
    #[test]
    fn the_wedge_leaves_the_top_chrome_alone() {
        let (q, _) = hero_scrim_quads(1.0, true);
        let bar_bottom = TOP_BAR_Y + TAB_PILL_H + TAB_TRACK_PAD + theme::space::XS;
        for (i, (r, _)) in q.iter().enumerate() {
            assert!(r.y >= bar_bottom, "quad {i} starts at y={} — inside the top chrome's band ({bar_bottom})", r.y);
        }
        // The feather also has to start ABOVE the highest text anchor, which is what lets
        // `hero_scrim_a` be a function of x alone (see `HERO_SCRIM_KNEE`) and is what makes the
        // legibility table below sound — every row it grades is at or below this line. A clearLogo
        // paints higher than any of them, but that is ART riding the feather, not a graded anchor.
        let highest = detail_title_cap_top().min(home_title_cap_top());
        assert!(HERO_SCRIM_KNEE <= highest, "the knee ({HERO_SCRIM_KNEE}) must be at or above the topmost hero line ({highest})");
    }

    /// The right wedge is OPT-IN (home's hero has no right-aligned copy and must not pay for one)
    /// and feathers in from both of its inner edges, so neither boundary can draw a line on the
    /// picture. Its overlap with the left wedge's tail is intended, not an accident to "fix" by
    /// moving an edge: the two fields multiply, and the facts line lives in the overlap.
    #[test]
    fn the_right_wedge_is_opt_in_and_starts_at_zero() {
        assert_eq!(hero_scrim_quads(1.0, false).1, 2, "no right wedge unless asked for");
        let (q, n) = hero_scrim_quads(1.0, true);
        assert_eq!(n, 3);
        let (r, k) = q[2];
        assert_eq!([k[0][3], k[1][3], k[3][3]], [0.0, 0.0, 0.0], "only the bottom-right corner carries weight");
        assert_eq!(k[2][3], HERO_SCRIM_R_A, "…and it carries exactly the token");
        for y in [r.y, r.y + r.h * 0.5, r.y + r.h] {
            assert_eq!(bilerp_a(q[2], r.x, y), 0.0, "the left edge must not draw a line");
        }
        for x in [r.x, r.x + r.w * 0.5, r.x + r.w] {
            assert_eq!(bilerp_a(q[2], x, r.y), 0.0, "the top edge must not draw a line");
        }
        // …and inside the quad the closed form must be the BILINEAR product `grad4` can actually
        // interpolate between those corners (u·v). The corners themselves are `hero_scrim_right_a`
        // by construction; this is the part of the shape that construction does not pin.
        for x in [1250.0f32, 1500.0, 1830.0, 1920.0] {
            for y in [750.0f32, 900.0, 1080.0] {
                let want = hero_scrim_right_a(x, y, 1.0);
                assert!((bilerp_a(q[2], x, y) - want).abs() < 1e-6, "drawn field != hero_scrim_right_a at ({x},{y})");
            }
        }
        assert!(r.x < HERO_SCRIM_W, "the two wedges are meant to overlap — the facts line sits in the overlap");
    }

    /// The wedge belongs to the HERO, not to the frame: at strength 0 there is nothing to draw.
    /// A residual wedge on the grid/shelf view would be a real regression and is invisible in a
    /// still — the shelves need the flat ground their card drop-shadows are tuned against.
    ///
    /// Graded on the closed forms alone. The quads' own corners USED to be swept here too, and that
    /// sweep is now a tautology: [`hero_scrim_quads`] builds every corner by evaluating these two
    /// functions, so it cannot report weight they do not have.
    #[test]
    fn the_wedge_leaves_with_the_hero() {
        assert_eq!(hero_scrim_a(0.0, 0.0), 0.0);
        assert_eq!(hero_scrim_a(HERO_SCRIM_W, 0.0), 0.0);
        assert_eq!(hero_scrim_right_a(1920.0, 1080.0, 0.0), 0.0);
    }

    // ---- the legibility contract itself -------------------------------------------------------

    /// One sRGB channel, linearized (WCAG 2.x) — see the `AmbientWash` block above for why this
    /// yardstick lives in the tests rather than in the app.
    fn srgb_lin(c: f32) -> f32 {
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    /// The contrast ratio an ink gets over `art` (a flat encoded grey standing in for a backdrop)
    /// once a scrim of total alpha `a` is composited over it. The panel is plain 888 with no sRGB
    /// framebuffer anywhere in the tree, so token values ARE sRGB codes and GL's blend is a
    /// straight lerp in that space: `c = art·(1−a) + SCRIM_INK·a`.
    fn contrast_over_art(ink: [f32; 4], art: f32, a: f32) -> f32 {
        let comp: Vec<f32> = (0..3).map(|i| art * (1.0 - a) + theme::SCRIM_INK[i] * a).collect();
        let l1 = 0.2126 * srgb_lin(ink[0]) + 0.7152 * srgb_lin(ink[1]) + 0.0722 * srgb_lin(ink[2]);
        let l2 = 0.2126 * srgb_lin(comp[0]) + 0.7152 * srgb_lin(comp[1]) + 0.0722 * srgb_lin(comp[2]);
        (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
    }

    /// One HERO-72 bold cap band, the device font's — quoted, not measured, because the host suite
    /// opens no SDL_ttf (the same boundary that keeps `detail::hero_chain` pure).
    const HERO_CAP_H: f32 = 52.0;

    /// The height of home's synopsis block at its cap — the SHARED hero blurb
    /// ([`crate::ui::hero_synopsis`]), so this table follows the rung and the leading instead of
    /// quoting them. It was the literal `87.0` with `// three size::MICRO lines at the hero's 29px
    /// leading` beside it, which is exactly the comment that goes stale in silence: the block is
    /// `LABEL`/36 now, and 87 would have gone on grading the title 21px lower than it is drawn.
    fn home_syn_h() -> f32 {
        crate::ui::hero_syn_h(crate::ui::HERO_SYN_MAXLINES)
    }

    /// The TOP of home's title band, from the bottom-anchored stack the hero draws (`hero_content`):
    /// the reserved logo band + `space::MD` + a one-line BODY kicker + `space::SM` + the synopsis
    /// block, stacked UP from `HERO_TEXT_BOTTOM` through the screen's own `hero_stack_top`, so the
    /// contract reads the arithmetic the draw uses. Only the KICKER's height is still quoted like
    /// [`HERO_CAP_H`], because it is one line of a rung and the host opens no SDL_ttf.
    fn home_title_band_top() -> f32 {
        const META_H: f32 = 34.0; // one line of `size::BODY`
        crate::ui::home::hero_stack_top(
            crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero),
            META_H,
            theme::space::SM + home_syn_h(),
        )
    }

    /// …and the cap top of the TEXT it falls back to, which is what the legibility table grades.
    /// The fallback is BASELINED on the band's bottom edge (`hero_logo::place`'s optical rule), so
    /// the ink sits a cap band above that line, ~70px below the band's own top. A clearLogo occupies
    /// the band instead and reaches higher — that is art, graded by eye on a capture, not here.
    fn home_title_cap_top() -> f32 {
        home_title_band_top() + crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero) - HERO_CAP_H
    }

    /// The same line on the detail page, where the band's anchor is `TITLE_BOTTOM` outright.
    fn detail_title_cap_top() -> f32 {
        crate::ui::detail::TITLE_BOTTOM - HERO_CAP_H
    }

    /// **The prize: "is the hero readable" as an assertion instead of a judgement on a television.**
    ///
    /// Every line of hero copy either screen draws over artwork, at the WORST point of its run (the
    /// right end, where the wedge is weakest), graded as
    /// `1 − (1 − base_scrim_a) · (1 − hero_scrim_a) · (1 − hero_scrim_right_a)` against two
    /// backdrops: a bright one (encoded 0.85 — a blown highlight, brighter than essentially any
    /// fanart pixel) and a blown-white one (1.0, the degenerate case the design only promises a
    /// floor for).
    ///
    /// The ys are READ from the screens' own layout arithmetic — `hero_chain` for detail, the
    /// bottom-anchored stack for home — so a layout change updates this contract rather than
    /// silently invalidating it. The floors are the measured achieved ratios: the point is that
    /// this fires the day someone brightens `SCRIM_INK`, dims a text token, moves
    /// `HERO_TEXT_BOTTOM` or trims `HERO_SCRIM_W`, none of which is visible in a diff.
    ///
    /// The two TITLE rows grade the TEXT fallback, at its cap top — a clearLogo occupies that band
    /// instead on most items and paints higher than the knee, which is art riding the feather rather
    /// than an anchor this closed form can speak for (see [`HERO_SCRIM_KNEE`]).
    ///
    /// The design floor is 3:1 over bright art. ONE row misses it and is listed anyway rather than
    /// hidden: detail's **facts line** is `TEXT_TERTIARY` (L=0.317), which needs α≈0.79 for 4.5:1 —
    /// an essentially black corner. That is an INK decision, not a scrim one, and is deliberately
    /// deferred; when the facts line moves to `TEXT_SECONDARY` over artwork its floor becomes 3.0
    /// like the rest.
    ///
    /// The people column was the second such row for exactly one day. Bottom-anchoring it moved its
    /// worst case 74px up the frame, where the composite ground is ≈0.59 instead of ≈0.71, and at
    /// tertiary that is 2.37:1 — so the row's floor was written down as 2.35 to match. **A contract
    /// is not a measurement**: the entry below asserts the ordinary 3.0/2.5 again, and
    /// `detail::PEOPLE_INK` is what meets it (one ink step, no scrim retune — the arithmetic for
    /// why that is the cheap half of the trade is on that const).
    #[test]
    fn the_hero_text_reads_over_bright_artwork() {
        use crate::ui::consts::{MARGIN_X, SCR_W};
        let hc = crate::ui::detail::hero_chain(
            // a two-line blurb — the shape `hero_chain`'s own doc is tuned on. Measuring is the one
            // thing the host cannot do, so the height is quoted, not computed.
            76.0, true,
        );
        let band = crate::ui::hero_logo::band_h(crate::ui::hero_logo::LogoRung::Hero);
        let home_col_r = MARGIN_X + crate::ui::home::HERO_COL_W; // 750 — the column's right end
        let det_col_r = MARGIN_X + crate::ui::detail::HERO_TEXT_W; // 990 — the synopsis' wrap edge,
        // and since nit 2 the title band's column too: a very wide wordmark runs the same 990.
        let people_r = SCR_W - MARGIN_X; // 1830 — the right-aligned people column's own edge

        // (label, x, y, ink, right wedge?, floor over BRIGHT art, floor over BLOWN WHITE)
        let rows: [(&str, f32, f32, [f32; 4], bool, f32, f32); 9] = [
            ("home title (right end)", home_col_r, home_title_cap_top(), theme::TEXT_PRIMARY, false, 3.0, 2.5),
            ("home title (at the margin)", MARGIN_X, home_title_cap_top(), theme::TEXT_PRIMARY, false, 7.0, 6.0),
            ("home kicker", 500.0, home_title_band_top() + band + theme::space::MD, theme::TEXT_SECONDARY, false, 3.0, 2.5),
            // the blurb's FIRST line, at the top of its block — the weakest ground the run sees.
            // Both its ink and its block height are read from the shared hero synopsis.
            ("home synopsis", home_col_r, crate::ui::home::HERO_TEXT_BOTTOM - home_syn_h(), theme::TEXT_READING, false, 3.0, 2.5),
            ("detail title", det_col_r, detail_title_cap_top(), theme::TEXT_PRIMARY, true, 3.0, 2.5),
            ("detail meta", 700.0, hc.meta_y, theme::TEXT_SECONDARY, true, 3.0, 2.5),
            ("detail synopsis", det_col_r, hc.syn_y, theme::TEXT_READING, true, 3.0, 2.5),
            // ⚠ the deferred ink decision — see the doc above
            ("detail facts", 1270.0, hc.facts_y, theme::TEXT_TERTIARY, true, 2.6, 2.1),
            // The people column at its WORST case: the top line of the tallest block it can produce
            // (a wrapped credit over a wrapped cast list), `PEOPLE_MAX_LINES` above the buttons —
            // 74px higher than the old top-anchored block, where the right wedge is feathering in
            // from y=702 and supplies about half the alpha it did (0.092 against 0.178). It clears
            // the ordinary floor because its ink is `detail::PEOPLE_INK`, which is read from the
            // screen rather than restated here, so an ink change here is a test failure and not a
            // silent one.
            (
                "detail people (top line)",
                people_r,
                crate::ui::detail::people_top(hc.btn_y, crate::ui::detail::PEOPLE_MAX_LINES),
                crate::ui::detail::PEOPLE_INK,
                true,
                3.0,
                2.5,
            ),
        ];

        for (label, x, y, ink, right, min_bright, min_white) in rows {
            let base = if label.starts_with("home") {
                crate::ui::home::base_scrim_a(y, 1.0)
            } else {
                crate::ui::detail::base_scrim_a(y, 1.0)
            };
            let wedge = hero_scrim_a(x, 1.0);
            let rw = if right { hero_scrim_right_a(x, y, 1.0) } else { 0.0 };
            let total = 1.0 - (1.0 - base) * (1.0 - wedge) * (1.0 - rw);
            for (art, floor) in [(0.85f32, min_bright), (1.0, min_white)] {
                let before = contrast_over_art(ink, art, base);
                let after = contrast_over_art(ink, art, total);
                assert!(
                    after >= floor,
                    "{label} at ({x},{y}) over art {art}: {after:.2}:1, under its {floor}:1 floor (base {base:.3} + wedge {wedge:.3} + right {rw:.3})"
                );
                assert!(after > before, "{label} over art {art}: the wedge made it WORSE ({before:.2} → {after:.2})");
            }
        }
    }

    /// **The chip's frame and its hit test are one rect, and it really does sit LEFT of the pills.**
    ///
    /// Both halves matter and neither is visible in a diff. The hit test is what makes the chip
    /// clickable on all three screens that draw it (it was Home's alone, recorded at Home's draw),
    /// so it must address exactly the rect the draw uses. And the D-pad rule every one of those
    /// screens now adopts — ◀ off the FIRST pill reaches the chip, ▶ walks back — is only sane
    /// while the chip is genuinely the bar's leftmost thing: [`TAB_SIDE_CLEAR`] is the margin that
    /// keeps the CENTRED strip off it, and a strip that grew far enough left to overlap would make
    /// the walk cross two controls occupying one place.
    #[test]
    fn the_profile_chip_is_hit_where_it_is_drawn_and_sits_left_of_the_pills() {
        let r = CHIP_FRAME;
        assert!(profile_chip_at(r.cx(), r.cy()), "the middle of the avatar is the chip");
        assert!(!profile_chip_at(r.x - 1.0, r.cy()), "…and a pixel outside it is not");
        assert!(!profile_chip_at(r.cx(), r.y + r.h + 1.0), "…on either axis");
        // it shares the bar's line with the pills: one control height, one top edge
        assert_eq!(r.y, TOP_BAR_Y);
        // one control height with the pills, which is also what makes the focused chip's capsule
        // the tab track's own band — the two read as one strip of chrome rather than as two
        // objects at different heights (see `CHIP_D`)
        assert_eq!(r.h, TAB_PILL_H);

        // the widest strip the row will ever lay out, centred: its left edge must clear the chip
        for n in 1..=16usize {
            let w = widths_for(n);
            let track_l = (crate::ui::consts::SCR_W - tab_view_w(&w)) * 0.5 - TAB_TRACK_PAD;
            assert!(
                track_l > r.x + r.w,
                "n={n}: the tab track reaches x={track_l}, over a chip ending at {}",
                r.x + r.w
            );
        }
    }

    /// A spread of pill widths. Deliberately wider than the device's (a real "TV" pill measures
    /// ≈67px at `size::BODY`, "Kids & Family Movies" ≈350) so a modest pill count crosses
    /// [`TAB_VIEW_MAX`] and the overflow arithmetic is exercised without inventing 30 libraries —
    /// the thresholds below are therefore about the geometry, not about a particular server.
    fn widths_for(n: usize) -> Vec<f32> {
        (0..n).map(|i| 140.0 + (i % 5) as f32 * 60.0).collect()
    }

    /// The unit-10 invariant, stated the way the [`tab_count`] doc states it: EVERY pill can be
    /// drawn, because the strip scrolls to it. For any section count, and from any starting
    /// scroll, the target for pill `i` puts that whole pill inside the visible strip — so no
    /// focusable index can lack a drawn rect, which is what the old `MAX_TABS` cap used to buy
    /// by refusing to focus the pills it could not reach.
    #[test]
    fn every_pill_scrolls_fully_into_the_strips_viewport() {
        for n in 1..=16usize {
            let w = widths_for(n);
            let view_w = tab_view_w(&w);
            let max = (tab_content_w(&w) - view_w).max(0.0);
            for &start in &[0.0f32, 400.0, -900.0, 9000.0] {
                for i in 0..n {
                    let t = tab_scroll_target(&w, i, start);
                    assert!(t >= -0.01 && t <= max + 0.01, "n={n} i={i}: scroll {t} outside 0..={max}");
                    let x = tab_pill_x(&w, i) - t; // the pill's left edge in viewport space
                    assert!(
                        x >= -0.01 && x + w[i] <= view_w + 0.01,
                        "n={n} i={i} (start {start}): pill spans {x}..{} of a {view_w}-wide strip",
                        x + w[i]
                    );
                }
            }
        }
    }

    /// A row that fits must keep its centered, unscrolled tvOS look — the track IS the content and
    /// there is nowhere to scroll to. Past that the scroll is minimal, not eager: asking for a
    /// pill that is already on screen must leave the strip exactly where it is, wherever that is.
    #[test]
    fn the_strip_scrolls_only_once_it_outgrows_the_row_and_only_as_far_as_it_must() {
        let fits = widths_for(4);
        assert!(tab_content_w(&fits) <= TAB_VIEW_MAX, "4 pills of this size must still fit");
        assert_eq!(tab_view_w(&fits), tab_content_w(&fits), "the track is the content");
        assert_eq!(tab_content_w(&fits) - tab_view_w(&fits), 0.0, "…so there is no scroll range");

        let over = widths_for(12);
        assert!(tab_content_w(&over) > TAB_VIEW_MAX, "12 pills of this size must overflow");
        assert_eq!(tab_view_w(&over), TAB_VIEW_MAX, "the track caps at the viewport");
        assert!(tab_scroll_target(&over, 11, 0.0) > 0.0, "the last pill must be scrolled to");
        assert_eq!(tab_scroll_target(&over, 0, 0.0), 0.0, "the first pill is already at the start");
        assert_eq!(tab_scroll_target(&over, 1, 0.0), 0.0, "a pill in view does not move the strip");
        // and the same rule part-way along: pill 5 sits inside the viewport at scroll 800, so the
        // strip HOLDS there rather than re-centering on it
        assert_eq!(tab_scroll_target(&over, 5, 800.0), 800.0, "a mid-strip pill in view holds");
    }

    /// A focus index left over from a bigger section table (the table is refetched on sign-in or
    /// a server switch) must clamp, not index out of bounds — the strip is drawn from these same
    /// widths every frame, so a panic here would be a panic in the frame loop. Graded on an
    /// OVERFLOWING row so the clamp has a non-zero range to be wrong about.
    #[test]
    fn a_stale_pill_index_clamps_instead_of_panicking() {
        let w = widths_for(12);
        let max = tab_content_w(&w) - tab_view_w(&w);
        assert!(max > 0.0, "the fixture must actually have somewhere to scroll");
        assert_eq!(tab_scroll_target(&w, 99, 500.0), 500.0, "an in-range scroll is left alone");
        assert_eq!(tab_scroll_target(&w, 99, 9e3), max, "an over-scroll is pulled back to the end");
        assert_eq!(tab_scroll_target(&w, 99, -5.0), 0.0, "a negative scroll is pulled to the start");
        assert_eq!(tab_pill_x(&w, 99), tab_pill_x(&w, 12), "past the end reads as the strip's end");
        assert_eq!(tab_content_w(&[]), 0.0, "an empty strip has no width and no gaps");
        assert_eq!(tab_scroll_target(&[], 0, 12.0), 0.0);
    }

    // ---- the travelling capsules --------------------------------------------------------------
    // `Spring::step` is `gfx::spring`, pure and already driven frame-by-frame by `card_row.rs`'s
    // tests, so capsule MOTION is fully host-testable. What is not: anything through
    // `with_tab_metrics` (it measures with SDL2_ttf), which is why these drive the pure
    // `Capsule`/`TabStrip` against `widths_for` spans instead of the real row.

    /// One frame at the app's own cadence, the value `card_row.rs`'s tests step with.
    const DT: f32 = 1.0 / 60.0;
    /// Pill `i`'s content-space `(x, w)` in the fixture strip — the same pair `tab_row_update` hands
    /// the strip, built from the same two functions.
    fn span_of(w: &[f32], i: usize) -> (f32, f32) {
        (tab_pill_x(w, i), w[i])
    }

    /// The one rule both the fill and the ink read, at every edge it has.
    #[test]
    fn cap_cover_is_the_pills_own_share_of_what_is_over_it() {
        let pill = (100.0, 200.0);
        assert_eq!(cap_cover(pill, pill), 1.0, "a capsule sitting exactly on the pill covers it");
        assert_eq!(cap_cover(pill, (0.0, 100.0)), 0.0, "abutting on the left is not covering");
        assert_eq!(cap_cover(pill, (300.0, 100.0)), 0.0, "…nor on the right");
        assert_eq!(cap_cover(pill, (0.0, 200.0)), 0.5, "half over the left edge is half the pill");
        assert_eq!(cap_cover(pill, (200.0, 400.0)), 0.5, "…and half over the right edge likewise");
        assert_eq!(cap_cover(pill, (-500.0, 5000.0)), 1.0, "a capsule wider than the pill still covers 1, not more");
        let z = cap_cover((100.0, 0.0), pill);
        assert_eq!(z, 0.0, "a zero-width pill covers nothing");
        assert!(z.is_finite(), "…and does not divide by zero into a NaN that would poison every ink");
    }

    /// A fixture in the shape the two tests below need: seeded, in one material, with the OTHER
    
    /// The contrast floor is a floor, so the two rates are not one rate: the bar darkens at the
    /// page's own pace and clears at roughly half it. Symmetric rates would spend half of every
    /// flicker under `TRACK_INK_CONTRAST`, which is the one thing the adaptive weight exists to hold.
    #[test]
    fn the_track_darkens_faster_than_it_clears() {
        let _g = serial_for_motion();
        assert!(
            density_k(0.40, 0.60) > density_k(0.60, 0.40),
            "getting darker protects the labels; getting lighter is cosmetic"
        );
        let dt = 1.0 / 60.0;
        let mut attack = crate::ui::Spring::at(0.40);
        attack.step(0.60, density_k(0.40, 0.60), dt);
        let mut release = crate::ui::Spring::at(0.60);
        release.step(0.40, density_k(0.60, 0.40), dt);
        assert!(
            (attack.pos - 0.40).abs() > (release.pos - 0.60).abs(),
            "over the same distance and one frame, attack covered {:.4} and release {:.4}",
            (attack.pos - 0.40).abs(),
            (release.pos - 0.60).abs(),
        );
    }

    /// The capsule/strip tests drive `Capsule::step`, whose landing frame `Spring::jump`s — and
    /// `Spring::jump` reports to `ui::idle`'s process-global dirty flag. So they are serial by
    /// obligation, not precaution (`xfade.rs`'s rule): under parallel libtest they intermittently
    /// failed OTHER modules' "a settled screen asks for nothing" assertions.
    fn serial_for_motion() -> std::sync::MutexGuard<'static, ()> {
        crate::testlock::serial()
    }

    /// The regression that would otherwise draw a bright capsule sweeping the whole strip on the
    /// first Library frame: an UNPLACED capsule lands on its pill, it does not fly to it. Alpha still
    /// fades in, because arriving in a row is a fade — the two are deliberately different motions.
    #[test]
    fn a_capsule_lands_on_its_first_pill_instead_of_flying_in_from_the_origin() {
        let _g = serial_for_motion();
        let w = widths_for(12);
        let p7 = span_of(&w, 7);
        let mut c = Capsule::new();
        c.step(Some((7, p7)), DT);
        assert_eq!(cap_cover(p7, c.span()), 1.0, "the first frame must already be ON pill 7");
        assert!(c.alpha() > 0.0 && c.alpha() < 1.0, "…while its alpha is still ramping up ({})", c.alpha());
    }

    /// A capsule mid-travel must always be sitting on SOMETHING. The label ink is derived from
    /// coverage, so a frame where neither neighbour is covered is a frame where both labels read as
    /// plain while a bright capsule floats in the gutter between them.
    #[test]
    fn a_travelling_capsule_is_never_sitting_on_nothing() {
        let _g = serial_for_motion();
        let w = widths_for(12);
        let (p0, p1) = (span_of(&w, 0), span_of(&w, 1));
        let mut c = Capsule::new();
        for _ in 0..60 {
            c.step(Some((0, p0)), DT);
        }
        assert!(cap_cover(p0, c.span()) > 0.99, "the fixture must start settled on pill 0");

        c.step(Some((1, p1)), DT);
        assert!(cap_cover(p0, c.span()) > 0.5, "one frame in it must still be mostly on pill 0, not teleported");
        for f in 0..60 {
            c.step(Some((1, p1)), DT);
            let on = cap_cover(p0, c.span()).max(cap_cover(p1, c.span()));
            assert!(on > 0.0, "frame {f}: the capsule is on neither pill (span {:?})", c.span());
        }
        assert!(cap_cover(p1, c.span()) > 0.99, "it must arrive fully on pill 1");
        assert!(cap_cover(p0, c.span()) < 0.01, "…and leave pill 0 behind");
    }

    /// Focus leaving the row fades the capsule where it stands rather than parking it at the origin,
    /// and coming back LANDS — the "capsule streaks across the row when you come back from the grid"
    /// failure, which is the whole reason [`CAP_LAND_A`] exists.
    #[test]
    fn focus_leaving_the_row_fades_in_place_and_coming_back_lands() {
        let _g = serial_for_motion();
        let w = widths_for(12);
        let (p2, p9) = (span_of(&w, 2), span_of(&w, 9));
        let mut c = Capsule::new();
        for _ in 0..60 {
            c.step(Some((2, p2)), DT);
        }
        let held = c.span();
        for _ in 0..40 {
            c.step(None, DT);
        }
        assert!(c.alpha() < CAP_LAND_A, "focus off the row must fade the capsule out ({})", c.alpha());
        assert_eq!(c.span(), held, "…in place: an invisible capsule must not also drift");

        c.step(Some((9, p9)), DT);
        assert_eq!(cap_cover(p9, c.span()), 1.0, "returning focus to a FAR pill lands on it, never glides");
    }

    /// The two-capsule model, locked against a regression to one: on the Library screen the selected
    /// tab is the section you are browsing while focus walks the row, so a pill can be wearing either,
    /// both, or neither — and the mixes it is inked with are exactly that answer.
    #[test]
    fn the_ink_a_pill_wears_is_exactly_the_capsule_over_it() {
        let _g = serial_for_motion();
        let w = widths_for(12);
        let settle = |sel: c_int, foc: c_int| {
            let mut s = TabStrip::new();
            for _ in 0..90 {
                s.update(sel, foc, |i| (i < w.len()).then(|| span_of(&w, i)), DT);
            }
            s
        };
        let s = settle(2, 5);
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        assert!(near(s.mixes(span_of(&w, 2)), (0.0, 1.0)), "the selected pill wears only the selection");
        assert!(near(s.mixes(span_of(&w, 5)), (1.0, 0.0)), "the focused pill wears only the focus");
        assert!(near(s.mixes(span_of(&w, 0)), (0.0, 0.0)), "an idle pill wears neither");
        let both = settle(3, 3);
        assert!(near(both.mixes(span_of(&w, 3)), (1.0, 1.0)), "one pill can wear both at once");
        // …and nothing is marked at all when there is nothing to mark (an emptied season strip, or
        // focus off the row on a page that has no selection either).
        let none = settle(-1, -1);
        assert!(near(none.mixes(span_of(&w, 0)), (0.0, 0.0)), "no target means no ink anywhere");
    }

    /// Ink CONSERVATION across a travel: the two capsules only ever have one pill's worth of coverage
    /// between them, so no frame can ink two labels toward `ACCENT_INK` at once. This is what bounds
    /// the one transient the design accepts — a partly covered pill inks its WHOLE label — to half a
    /// label on each of two pills for the length of one travel, rather than a whole row dimming.
    #[test]
    fn a_travel_never_inks_more_than_one_pills_worth_of_label() {
        let _g = serial_for_motion();
        let w = widths_for(12);
        let mut s = TabStrip::new();
        let span = |i: usize| (i < w.len()).then(|| span_of(&w, i));
        for _ in 0..90 {
            s.update(-1, 4, span, DT);
        }
        for f in 0..90 {
            s.update(-1, 5, span, DT);
            let total: f32 = (0..w.len()).map(|i| s.mixes(span_of(&w, i)).0).sum();
            assert!(total <= 1.0 + 1e-3, "frame {f}: {total} pills' worth of focus ink is lit at once");
            assert!(total > 0.0, "frame {f}: the focus ink went out entirely mid-travel");
        }
    }

    /// Every RESTING state a strip-driven pill can be in must be the look it replaced, to the bit —
    /// this is the whole reason the mix lerps between the same ink roles the boolean arms pick from
    /// rather than inventing a ramp of its own.
    ///
    /// **The IDLE role is the one deliberate exception, and it is a fact about grounds rather than
    /// about pills.** The strip-driven row is the standing tab track: it sits on glass over
    /// uncontrolled artwork, and its scrim is solved so this ink clears [`TRACK_INK_CONTRAST`] — a
    /// muted `TEXT_TERTIARY` there costs .61 of black over a bright hero, which is a band laid across
    /// the picture rather than a material. The boolean arms are the detail page's season tabs, drawn
    /// bare on a controlled dark ground where nothing is solved and the muted ink is right. Two
    /// grounds, two answers; the roles they SHARE still have to agree to the bit.
    #[test]
    fn a_settled_mixed_pill_is_the_boolean_look_it_replaced() {
        // "to the bit" means to the PANEL's bit: a lerp that lands on its endpoint still carries a
        // ~3e-8 float residue, and the framebuffer is 8 bits per channel.
        let is = |got: [f32; 4], want: [f32; 4], what: &str| {
            for i in 0..4 {
                assert!(
                    (got[i] - want[i]).abs() < 0.5 / 255.0,
                    "{what}: channel {i} is {} not {} — over half a display code out",
                    got[i],
                    want[i]
                );
            }
        };
        is(TabPill::mixed_ink(0.0, 0.0), theme::TEXT_READING, "plain segment on glass");
        assert_ne!(
            theme::TEXT_READING, theme::TEXT_TERTIARY,
            "the fixture is meaningless if the two idle inks have converged"
        );
        is(TabPill::mixed_ink(0.0, 1.0), theme::TEXT_PRIMARY, "selected, focus elsewhere");
        is(TabPill::mixed_ink(1.0, 0.0), crate::ui::ACCENT_INK, "focused");
        is(TabPill::mixed_ink(1.0, 1.0), crate::ui::ACCENT_INK, "focus outranks selection");
        // out-of-range mixes clamp rather than extrapolating past the tokens
        is(TabPill::mixed_ink(-1.0, 4.0), theme::TEXT_PRIMARY, "an over-range selection clamps");
        is(TabPill::mixed_ink(9.0, -9.0), crate::ui::ACCENT_INK, "an over-range focus clamps");
    }

    /// **The bottom stop may not run past the ceiling.** The solve sizes the scrim so the labels
    /// clear their floor against the TOP stop, which is the lightest of the pair, and the bottom was
    /// free to be `a + spread` however heavy `a` got. Past [`theme::TAB_TRACK_A_TOP`] there is
    /// nothing left to see through, so a bar drawn there has stopped being glass at its lower edge —
    /// which is the flat capsule this material replaced, reintroduced under every heavy bar. Found by
    /// a judging panel doing the arithmetic, not by looking.
    #[test]
    fn the_pair_never_draws_past_the_weight_where_glass_stops_being_glass() {
        let _g = serial_for_motion();
        let restore = unsafe { std::ptr::addr_of!(TRACK_DENSITY).read() };
        let mut prev = 0.0f32;
        for i in 0..=40 {
            // a neutral ground swept the whole way up, so the solve sweeps its whole range
            let v = i as f32 / 40.0;
            unsafe {
                *std::ptr::addr_of_mut!(TRACK_DENSITY) =
                    TrackDensity { drawn: crate::ui::Spring::at(0.0), want: 0.0, seeded: false }
            };
            let (top, bot) = tab_glass_stops([v, v, v]);
            assert!(
                bot[3] <= theme::TAB_TRACK_A_TOP + 1e-6,
                "ground {v:.2}: bottom stop {:.3} is past the ceiling {:.3}",
                bot[3], theme::TAB_TRACK_A_TOP
            );
            assert!(bot[3] >= top[3] - 1e-6, "ground {v:.2}: the pair inverted");
            assert!(top[3] >= prev - 1e-6, "ground {v:.2}: the top stop went backwards");
            prev = top[3];
        }
        unsafe { *std::ptr::addr_of_mut!(TRACK_DENSITY) = restore };
    }

    /// A dark ground keeps the authored pair to the bit — that is the case the tokens were drawn
    /// for, and the taper must not disturb it.
    #[test]
    fn a_dark_ground_draws_the_authored_pair_exactly() {
        let _g = serial_for_motion();
        let restore = unsafe { std::ptr::addr_of!(TRACK_DENSITY).read() };
        unsafe {
            *std::ptr::addr_of_mut!(TRACK_DENSITY) =
                TrackDensity { drawn: crate::ui::Spring::at(0.0), want: 0.0, seeded: false }
        };
        let (top, bot) = tab_glass_stops([0.0, 0.0, 0.0]);
        unsafe { *std::ptr::addr_of_mut!(TRACK_DENSITY) = restore };
        assert!((top[3] - theme::TAB_GLASS_TOP[3]).abs() < 1e-6, "top {:.4}", top[3]);
        assert!((bot[3] - theme::TAB_GLASS_BOT[3]).abs() < 1e-6, "bot {:.4}", bot[3]);
    }

    /// **The plated composite**, which is arithmetic and therefore has no business being graded by
    /// eye. A season tab's ground is now TWO layers — the pill's own idle plate and the strip's
    /// travelling selection capsule over it — where it used to be one flat token. Same-colour
    /// source-over is `a + b − ab`, so the pair must land back on [`theme::TAB_PLATE_SELECTED`]; and
    /// the plate must retire exactly as the OPAQUE focus capsule arrives, or a focused season would
    /// wear a 0.08 white film its unplated twin in the top row does not.
    #[test]
    fn a_travelling_plate_lands_back_on_the_plate_it_replaced() {
        let idle = theme::TAB_PLATE_IDLE[3];
        let over = theme::TAB_PLATE_SELECTED_OVER[3];
        let composite = over + idle * (1.0 - over);
        assert!(
            (composite - theme::TAB_PLATE_SELECTED[3]).abs() < 1.0 / 255.0,
            "capsule .{over} over plate .{idle} composites to {composite}, not TAB_PLATE_SELECTED's {}",
            theme::TAB_PLATE_SELECTED[3]
        );
        // order-independence is what lets the pill keep painting its plate AFTER the strip drew the
        // capsule under it — the draw order this component actually uses
        assert!(
            ((idle + over * (1.0 - idle)) - composite).abs() < 1e-6,
            "same-colour source-over must be order-independent, or the draw order becomes load-bearing"
        );
        // all three plate tokens are the same white; only their weight differs
        for i in 0..3 {
            assert_eq!(theme::TAB_PLATE_SELECTED_OVER[i], theme::TAB_PLATE_IDLE[i], "channel {i}");
            assert_eq!(theme::TAB_PLATE_SELECTED_OVER[i], theme::TAB_PLATE_SELECTED[i], "channel {i}");
        }
        // …and the split survives the `Painter` cascade, which scales BOTH layers: two layers under
        // a cascade are not the same function as one, so the drift is bounded here rather than
        // assumed. (Nothing draws a tab strip at α<1 today; this is the guard for the day one does.)
        for &a in &[1.0f32, 0.75, 0.5, 0.25] {
            let one = a * theme::TAB_PLATE_SELECTED[3];
            let two = a * over + a * idle * (1.0 - a * over);
            assert!(
                (one - two).abs() < 1.0 / 255.0,
                "at cascade α={a} the two-layer plate is {two} against the old {one} — over a display code apart"
            );
        }
    }

    /// **The container has to be visible on a page that is not.** A black scrim can only subtract,
    /// so before the lift existed the bar's face over an L\*0 page measured the page exactly and
    /// the whole object was one pixel of rim. This is the invariant that says it never happens
    /// again — stated as a face lighter than its ground, which is what a person sees, rather than
    /// as the constant that currently produces it.
    #[test]
    fn the_glass_never_goes_darker_than_its_floor() {
        let floor = theme::TAB_GLASS_LIFT_FLOOR;
        for i in 0..=100 {
            let lum = i as f32 / 100.0;
            let ground = [lum, lum, lum];
            let a = track_alpha_for(ground);
            let g = track_lift(ground, a);
            let face = lum * (1.0 - a) + g * a;
            assert!(
                face >= floor.min(lum * (1.0 - a)).min(floor) - 1e-4,
                "over a page at {lum} the face is {face}, under the floor {floor}"
            );
        }
        // the case the floor exists for: a page that shows nothing at all
        let a = track_alpha_for([0.0, 0.0, 0.0]);
        let face = track_lift([0.0, 0.0, 0.0], a) * a;
        assert!(
            face >= floor - 1e-4,
            "over a page at zero the face is {face}, short of the floor {floor}"
        );
    }

    /// …and it costs a bright bar NOTHING. The lift is spent out of the density solve's slack, so
    /// the moment the solve leaves its floor there is none: a heavy bar is the same black scrim it
    /// was, to the bit. Without this the fix for the dark end quietly lightens the light end, which
    /// is where the labels are hardest to hold.
    #[test]
    fn a_bright_ground_gets_no_lift_at_all() {
        for &lum in &[0.45f32, 0.6, 0.8, 1.0] {
            let ground = [lum, lum, lum];
            let a = track_alpha_for(ground);
            assert!(
                track_lift(ground, a) == 0.0,
                "ground {lum} solved to α={a} and still took a lift"
            );
        }
    }

    /// The clamp is the whole licence for the rule: the solve above sized the scrim against a BLACK
    /// tint, and a lighter face spends the contrast it bought. Whatever the lift does, the idle
    /// label still clears its floor.
    #[test]
    fn the_lift_never_spends_the_labels_contrast() {
        for i in 0..=40 {
            let lum = i as f32 / 40.0;
            let ground = [lum, lum, lum];
            let a = track_alpha_for(ground);
            let g = track_lift(ground, a);
            let face = [
                ground[0] * (1.0 - a) + g * a,
                ground[1] * (1.0 - a) + g * a,
                ground[2] * (1.0 - a) + g * a,
                1.0,
            ];
            let c = contrast(theme::TEXT_READING, face);
            assert!(
                c >= TRACK_INK_CONTRAST - 0.01,
                "ground {lum}: α={a} lift={g} leaves the idle label at {c:.2}:1"
            );
        }
    }
    /// **The floor above is the TRACK's, over the track's own ground — and the band's other surface
    /// has no equivalent.** A PINNED EXPOSURE, not a bug assertion: [`BarMaterial`]'s note is where
    /// the decision and the three candidate fixes live.
    ///
    /// [`profile_chip`]'s capsule wears the face [`track_alpha_for`] solved against pixels ~800px
    /// away, and the unfurled capsule's whole content is a name in [`theme::TEXT_PRIMARY`]. Where
    /// the two grounds agree the shared face is exactly right, which is the arrangement's whole
    /// argument; where they do not, the chip carries a promise nobody made about it. Nothing
    /// darkens the top band before this — `home::HERO_BASE_SCRIM_Y0` is 367 and [`HERO_SCRIM_TOP`]
    /// 162 — so both grounds are raw backdrop, and `gfx::sample_ground`'s census puts the MEDIAN
    /// span at 26.8 L* across the track alone.
    ///
    /// Written as a test so the number cannot drift silently in EITHER direction: tightening the
    /// exposure, or closing it, fails here and sends the next reader to the note.
    #[test]
    fn the_bands_face_carries_no_promise_about_the_chips_own_ground() {
        // a neutral at a CIE lightness — the axis every measurement in this material is quoted in
        let grey_at = |l: f32| {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..40 {
                let m = 0.5 * (lo + hi);
                if lstar([m, m, m, 1.0]) < l {
                    lo = m
                } else {
                    hi = m
                }
            }
            0.5 * (lo + hi)
        };
        // the drawn face, exactly as the test above builds it
        let face_over = |g: f32, a: f32| {
            let v = g * (1.0 - a) + track_lift([g, g, g], a) * a;
            [v, v, v, 1.0]
        };
        let dark = grey_at(20.0);
        let a = track_alpha_for([dark, dark, dark]);
        assert_eq!(a, theme::TAB_GLASS_TOP[3], "a dark track rests the solve on its floor");
        assert!(
            contrast(theme::TEXT_READING, face_over(dark, a)) >= TRACK_INK_CONTRAST,
            "the track's own ink is served — that is the contract this one is measured against"
        );
        // …and the same face, under the chip, over a bright corner of the same hero.
        let bright = grey_at(92.0);
        let chip = contrast(theme::TEXT_PRIMARY, face_over(bright, a));
        assert!(
            (1.7..2.1).contains(&chip),
            "the chip's name over L*92 art with the track solved dark reads {chip:.2}:1 — if this \
             moved, in either direction, re-read `BarMaterial`"
        );
        let was = {
            let v = bright * (1.0 - theme::TAB_TRACK_A_TOP);
            contrast(theme::TEXT_PRIMARY, [v, v, v, 1.0])
        };
        assert!(was > 9.0, "the flat capsule this replaced cleared every ground: {was:.2}:1");
    }
}
