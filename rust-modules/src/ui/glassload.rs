//! `glassload` — the dev-only LOAD DIAL for the backdrop-glass chain, and the blurred-route-
//! transition prototype it was built beside.
//!
//! Everything here exists to answer one question the shipped scenes cannot: **where is the cliff?**
//! The Account panel and the tab track are two fixed surfaces at two fixed sizes; measuring them
//! tells you what today costs, not what the hardware will take. So this module turns the four
//! variables that actually move the price into a dial a run can sweep:
//!
//! * **how many** glass surfaces are on the panel,
//! * **how large** each one is (and therefore how large the one shared snapshot region is),
//! * **how often** the snapshot refreshes, in presents,
//! * how large the one shared snapshot region ends up, which is what actually sets the price.
//!
//! ## Why it is a SWEEP rather than a flag
//!
//! `docs/backdrop-blur-profiling.md` rule 3: the television drifts 60 → 50 fps over a session, so
//! legs must be interleaved, never run AAA then BBB. A dial that has to be re-armed between legs
//! makes interleaving a scripting problem and a deploy per leg. This one cycles its own steps on a
//! timer inside ONE launch, so a four-configuration sweep held six seconds a step and cycled five
//! times is interleaved by construction, and every sample is drawn from the same thermal state
//! within a couple of minutes.
//!
//! The current step index rides the once/sec heartbeat as `load=<i>` and every HWCNT phase record
//! as `"load":<i>`, which is what lets one log be split into per-configuration distributions after
//! the fact. `tools/analyze-loadsweep.py` is the reader.
//!
//! ## The grammar
//!
//! `/tmp/plxnative-glassload` holds an optional `hold=<seconds>;` prefix and then a comma-separated
//! list of steps:
//!
//! ```text
//! hold=6;off,1x608x396@3,2x608x396@3,1x1920x1080@3,1x1920x1080@1
//! ```
//!
//! A step is one of three things:
//!
//! * `off` — no extra draws at all: the control leg,
//! * `<count>x<width>x<height>@<cadence>` — that many GLASS surfaces, where the cadence is
//!   **presents between snapshot refreshes**: `@1` refreshes every presented frame, `@3` is the
//!   shipped dynamic policy, `@0` captures once and caches forever,
//! * `c<count>x<width>x<height>` (or `cf…` for the FOCUSED penumbra) — that many CARD composites,
//!   the `draw_tex_carded` path every art tile takes,
//! * `acct` — the REAL shipped Account popover, presented as its own `ModalStack` surface
//!   (`bridge::open_account_menu`) over the real Home page, so
//!   a synthetic surface and the thing it stands for can be interleaved in ONE launch. This step
//!   exists because a dial panel and the shipped panel were read as the same configuration by two
//!   different agents and disagreed by 15 fps; §"the reconciliation" in
//!   `docs/glass-hardware-budget.md` is what it settled.
//!
//! Any step may carry a leading `s` — **spread**: instead of tiling adjacently in the middle, the
//! surfaces are pushed apart along the diagonal, corner to corner. Same count, same area, and a
//! blurred REGION that grows to most of the screen, because the one shared snapshot has to hold
//! every glass surface in the frame and everything between them.
//!
//! The card form is the second axis this dial exists for. A sibling measurement priced a card
//! fragment at **3.02 GPU cycles and 5.86 arithmetic words** — 3x a textured full-screen quad and
//! 120x a gradient — from four peek-row tiles covering 13.6% of the panel. Card area is the one
//! quantity design controls directly and can scale freely, so "what happens when the panel is
//! mostly cards" needed a dial rather than an extrapolation. Cards here sample four synthetic
//! poster-sized textures in rotation, allocated once; see [`card_tex`] for why not one.
//!
//! Panels are tiled adjacently in a centred grid, so `n` surfaces cost `n` composites inside ONE
//! grown region — which is the shape a real screen has, and the shape `gfx::blur_region_union`
//! is designed around. Two surfaces at opposite corners are a different (worse) case and are not
//! what this dial produces; that asymmetry is recorded in `docs/glass-hardware-budget.md`.
//!
//! ## The transition prototype (`/tmp/plxnative-navblur`)
//!
//! The route change is a DIP today (see [`crate::ui::nav`]): the outgoing page fades to
//! `theme::SURFACE_APP`, the route flips at the floor, the incoming page fades up. The design
//! question is whether that grey trough can be a BLUR of the outgoing page with the tab bar's
//! glass sitting on top of it. Armed, this module holds the page at full alpha and cross-fades a
//! full-bleed glass surface over it instead, with a tab-track-shaped capsule composited above —
//! and the capsule samples the SAME cache, which is the whole compositional finding.
//!
//! Content is `<mode>[p][:<cadence>]`. Mode empty or `1` = one shared snapshot (what the
//! architecture does today); `2` = force a SECOND snapshot between the two surfaces, which is what
//! a private cache per surface would cost in cycles. A trailing `p` PINS the slab at full strength
//! instead of riding the transition — a 210 ms fade gives a handful of frames to measure and is
//! nearly impossible to photograph, so the pinned form is what a cost run and a capture use, and
//! the unpinned form is what proves the motion reads. Both are behind the trigger; neither changes
//! a default build.
//!
//! Main-thread only. The live state is [`Dial`], a field of the frame plan
//! ([`crate::ui::frame::glass::GlassPlan`]) since phase 11 rather than the eight `static mut`s it
//! was before; the two things instruments read with no borrow of the plan in hand — the live step
//! index and whether anything is armed — are PUBLISHED snapshots the dial writes on change
//! (spec §2.3). Every entry point is a no-op when the trigger is absent, and in a
//! `--no-default-features` build `dev::read` is `None` at compile time, so nothing here can be
//! reached at all.

use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::theme;
use crate::ui::{Painter, Rect};

/// What a step's surfaces ARE. Both take the same count/size/layout; they differ only in which
/// shader path the composite goes down, which is the whole point of having both on one dial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `fs_glass` over the shared blurred snapshot — the backdrop panel.
    Glass,
    /// `draw_tex_carded` — texture + edge sheen + folded drop shadow. `true` = drawn at the FOCUSED
    /// pop, which grows the penumbra and so the rasterized quad.
    Card { focused: bool },
    /// `draw_tex` at radius 0 — the FLAT textured quad, the cheapest full-coverage path the app
    /// has and the one the hero photograph takes. The control the other two are priced against.
    Image,
    /// Not a synthetic surface at all: the REAL Account popover, on the real route, with cached
    /// panel glass and its one-copy host [`crate::gfx::FrameCache`]. Its geometry and lifecycle are
    /// the shipped ones; this module only asks for the route. Size fields are ignored.
    Account,
}

/// One configuration of the dial.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Step {
    /// Which composite these surfaces are.
    pub(crate) kind: Kind,
    /// Push the surfaces apart to opposite corners instead of tiling them adjacently, so the one
    /// shared blur region has to span them and everything in between.
    pub(crate) spread: bool,
    /// How many surfaces. Zero is the control leg — the page with nothing extra on it at all.
    pub(crate) n: u32,
    /// Each surface's authored size. The snapshot region is the union of these grown by
    /// `gfx::BLUR_MARGIN` (88) on every side and clamped to the screen.
    pub(crate) w: f32,
    pub(crate) h: f32,
    /// Presents between snapshot refreshes. 0 = capture once and never again.
    pub(crate) cadence: u32,
}

impl Step {
    const OFF: Self = Self {
        kind: Kind::Glass,
        spread: false,
        n: 0,
        w: 0.0,
        h: 0.0,
        cadence: 0,
    };

    /// Total composite area this step submits, in authored px — the x axis of the scaling law.
    /// NOMINAL: a card's quad is additionally inflated by its penumbra, and a glass panel's
    /// snapshot region by `gfx::BLUR_MARGIN`, neither of which is included here.
    pub(crate) fn px(self) -> f64 {
        self.n as f64 * self.w as f64 * self.h as f64
    }
}

/// A parsed sweep: how long each step is held, and the steps themselves.
pub(crate) struct Sweep {
    hold_ms: u32,
    steps: Vec<Step>,
}

/// Corner radius every load-dial panel is drawn with — the popover radius, so the composite is the
/// same shader path a real panel takes (rounded SDF, bevel, rim, interior early-out).
const PANEL_RADIUS: f32 = 28.0;
/// Gap between tiled panels, authored px. Small enough that `n` panels stay inside one grown
/// region rather than pulling the union out to the whole screen.
const PANEL_GAP: f32 = 24.0;

/// Parse the trigger's content. Returns `None` when there is nothing usable in it, so a typo
/// disarms the dial loudly (the caller logs) instead of half-arming it.
pub(crate) fn parse(spec: &str) -> Option<Sweep> {
    let mut hold_ms = 6000u32;
    let mut body = spec.trim();
    if let Some(rest) = body.strip_prefix("hold=") {
        let (secs, rest) = rest.split_once(';')?;
        hold_ms = (secs.trim().parse::<f32>().ok()? * 1000.0) as u32;
        body = rest.trim();
    }
    if hold_ms < 1000 {
        return None; // shorter than the 1 Hz heartbeat is a step nothing can be attributed to
    }
    let mut steps = Vec::new();
    for tok in body.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        if tok.eq_ignore_ascii_case("off") {
            steps.push(Step::OFF);
            continue;
        }
        if tok.eq_ignore_ascii_case("acct") {
            steps.push(Step {
                kind: Kind::Account,
                ..Step::OFF
            });
            continue;
        }
        // `s` = spread, before the kind prefix. Stripped first so `sc4x…` reads as spread cards.
        let (spread, tok) = match tok.strip_prefix('s') {
            Some(r) => (true, r),
            None => (false, tok),
        };
        // `cf` before `c`: the longer prefix has to win, or a focused-card step parses as an
        // ordinary one whose count begins with `f` and the whole spec is rejected.
        let (kind, tok) = if let Some(r) = tok.strip_prefix("cf") {
            (Kind::Card { focused: true }, r)
        } else if let Some(r) = tok.strip_prefix('c') {
            (Kind::Card { focused: false }, r)
        } else if let Some(r) = tok.strip_prefix('i') {
            (Kind::Image, r)
        } else {
            (Kind::Glass, tok)
        };
        let (geom, cad) = tok.split_once('@').unwrap_or((tok, "1"));
        let mut parts = geom.split('x');
        let n: u32 = parts.next()?.trim().parse().ok()?;
        let w: f32 = parts.next()?.trim().parse().ok()?;
        let h: f32 = parts.next()?.trim().parse().ok()?;
        if parts.next().is_some() || n == 0 || w < 1.0 || h < 1.0 {
            return None;
        }
        steps.push(Step {
            kind,
            spread,
            n,
            w: w.min(SCR_W),
            h: h.min(SCR_H),
            cadence: cad.trim().parse().ok()?,
        });
    }
    (!steps.is_empty()).then_some(Sweep { hold_ms, steps })
}

/// Where `step`'s panels sit: a centred grid, row-major, clipped to what fits on the screen.
///
/// Pure, so the packing is host-gradeable — a dial that silently drew four panels where the spec
/// asked for eight would report a cost for a configuration that never ran.
pub(crate) fn layout(step: Step) -> Vec<Rect> {
    if step.n == 0 || matches!(step.kind, Kind::Account) {
        return Vec::new();
    }
    let w = step.w.min(SCR_W);
    let h = step.h.min(SCR_H);
    if step.spread {
        // Evenly along the top-left → bottom-right diagonal, so two surfaces land in opposite
        // corners and the union of their blur regions is very nearly the whole canvas.
        let last = (step.n - 1).max(1) as f32;
        return (0..step.n)
            .map(|i| {
                let t = if step.n == 1 { 0.5 } else { i as f32 / last };
                Rect::new((SCR_W - w) * t, (SCR_H - h) * t, w, h)
            })
            .collect();
    }
    let fit =
        |span: f32, size: f32| ((span + PANEL_GAP) / (size + PANEL_GAP)).floor().max(1.0) as u32;
    let cols = fit(SCR_W, w).min(step.n);
    let rows_fit = fit(SCR_H, h);
    let rows = step.n.div_ceil(cols).min(rows_fit);
    let total = (cols * rows).min(step.n);
    let block_w = cols as f32 * w + (cols - 1) as f32 * PANEL_GAP;
    let block_h = rows as f32 * h + (rows - 1) as f32 * PANEL_GAP;
    let x0 = ((SCR_W - block_w) * 0.5).max(0.0);
    let y0 = ((SCR_H - block_h) * 0.5).max(0.0);
    (0..total)
        .map(|i| {
            let (c, r) = (i % cols, i / cols);
            Rect::new(
                x0 + c as f32 * (w + PANEL_GAP),
                y0 + r as f32 * (h + PANEL_GAP),
                w,
                h,
            )
        })
        .collect()
}

// ---- live state ------------------------------------------------------------------------------

/// **The dial's whole live state, as fields.** Eight `static mut`s until phase 11; owned by
/// [`crate::ui::frame::glass::GlassPlan`] since, which is what `ci/allow/statics.txt` promised
/// when it entered them with the reason "owned by the frame plan from phase 11".
///
/// Main render thread only, like the snapshot chain it schedules.
const NCARD_TEX: usize = 4;

pub(crate) struct Dial {
    sweep: Option<Sweep>,
    /// Which step is live, or -1 when the dial is disarmed. Mirrored into [`STEP_PUB`] on every
    /// change, because the heartbeat and every HWCNT phase record read it with no `&App` in hand.
    step: i32,
    /// Presented frames since the current step began — the cadence clock.
    presents: u32,
    /// `SDL_GetTicks` at the first prepared frame, so step boundaries are wall-clock and
    /// independent of how many frames the set managed to draw.
    t0: u32,
    /// The navblur prototype's mode: 0 = off, 1 = one shared snapshot, 2 = a private second one.
    navblur: u32,
    /// Presents between navblur snapshot refreshes.
    navblur_cadence: u32,
    /// Hold the slab at full strength rather than riding the transition — see the module doc.
    navblur_pin: bool,
    /// The synthetic poster textures the card steps sample — see [`Dial::card_tex`].
    card_tex: [u32; NCARD_TEX],
}

/// **The live step index, PUBLISHED** (spec §2.3): the dial owns the decision and writes this on
/// every change; instruments read it freely. `ui/profile.rs` tags every HWCNT phase record with
/// it from inside an arbitrary draw closure and has no borrow of the plan to ask through, and a
/// cycled run that could not be split by step after the fact reports a mean over configurations
/// that never occurred.
static STEP_PUB: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
/// Whether anything in this module is armed, published for the same readers and the same reason.
static ARMED_PUB: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Dial {
    pub(crate) const fn new() -> Self {
        Self {
            sweep: None,
            step: -1,
            presents: 0,
            t0: 0,
            navblur: 0,
            navblur_cadence: 3,
            navblur_pin: false,
            card_tex: [0; NCARD_TEX],
        }
    }

    /// Mirror the two decisions instruments read into their published snapshots. There is exactly
    /// one `Dial` in the process (the one on `App`'s frame plan), so the last one to change owns
    /// the publication — constructing a plan therefore publishes too, which is what keeps a host
    /// test that arms one from leaving the instruments armed for every test after it.
    pub(crate) fn publish(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        STEP_PUB.store(self.step, Relaxed);
        ARMED_PUB.store(self.armed(), Relaxed);
    }

    /// Arm the dial from `/tmp/plxnative-glassload`'s content. Logs what it will actually run, because
    /// a sweep that silently dropped a step reports a curve with a hole in it.
    pub(crate) fn configure(&mut self, spec: &str) {
        match parse(spec) {
            Some(s) => {
                let list: Vec<String> = s
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(i, st)| {
                        if st.n == 0 {
                            return format!("{i}:off");
                        }
                        let drawn = layout(*st).len();
                        // The DRAWN count and the drawn AREA, never the requested ones: a step whose
                        // panels did not fit would otherwise report a cost for a scene that never ran.
                        let px = drawn as f64 * st.w as f64 * st.h as f64;
                        if matches!(st.kind, Kind::Account) {
                            return format!("{i}:acct (the real shipped popover)");
                        }
                        let sp = if st.spread { " spread" } else { "" };
                        match st.kind {
                            Kind::Account => unreachable!("handled above"),
                            Kind::Glass => format!(
                                "{i}:glass{sp} {}x{}x{}@{} drawn={drawn} px={px:.0}",
                                st.n, st.w, st.h, st.cadence
                            ),
                            Kind::Card { focused } => format!(
                                "{i}:card{}{sp} {}x{}x{} drawn={drawn} px={px:.0}",
                                if focused { "-focused" } else { "" },
                                st.n,
                                st.w,
                                st.h
                            ),
                            Kind::Image => {
                                format!(
                                    "{i}:image{sp} {}x{}x{} drawn={drawn} px={px:.0}",
                                    st.n, st.w, st.h
                                )
                            }
                        }
                    })
                    .collect();
                crate::log(&format!(
                    "GLASSLOAD armed hold={}ms steps=[{}]",
                    s.hold_ms,
                    list.join(" ")
                ));
                self.sweep = Some(s);
                self.step = 0;
                self.publish();
            }
            None => crate::log(&format!(
                "GLASSLOAD spec {spec:?} not understood — dial disarmed"
            )),
        }
    }
}

/// Arm the blurred-transition prototype from `/tmp/plxnative-navblur`'s content
/// (`[<mode>][:<cadence>]`, mode 1 or 2, cadence in presents).
pub(crate) fn parse_navblur(spec: &str) -> Option<(u32, u32, bool)> {
    let (head, cad) = spec.trim().split_once(':').unwrap_or((spec.trim(), "1"));
    let pin = head.ends_with('p') || head.ends_with('P');
    let head = head.trim_end_matches(['p', 'P']);
    let mode: u32 = if head.is_empty() {
        1
    } else {
        head.parse().ok()?
    };
    let cad: u32 = cad.trim().parse().ok()?;
    (1..=2).contains(&mode).then_some((mode, cad, pin))
}

impl Dial {
    pub(crate) fn configure_navblur(&mut self, spec: &str) {
        let Some((mode, cad, pin)) = parse_navblur(spec) else {
            crate::log(&format!(
                "NAVBLUR spec {spec:?} not understood — prototype off"
            ));
            return;
        };
        self.navblur = mode;
        self.navblur_cadence = cad;
        self.navblur_pin = pin;
        self.publish();
        // Pinned, the slab is not a transition at all, so the page must keep its own fade. Only the
        // riding form replaces the dip.
        crate::ui::nav::set_blur_dissolve(!pin);
        crate::log(&format!(
            "NAVBLUR armed mode={mode} cadence={cad} pinned={pin}"
        ));
    }

    /// Does the live step want the REAL Account popover open? Read by `app.rs`, which owns `Route`.
    ///
    /// A dev step that changes the ROUTE rather than drawing something is unusual, and it is the only
    /// way to put the shipped surface and a synthetic one in the same interleaved launch — which is
    /// exactly what was needed when two agents read the same nominal configuration 15 fps apart.
    pub(crate) fn wants_account(&self) -> bool {
        let Some(s) = self.sweep.as_ref() else {
            return false;
        };
        self.step >= 0 && matches!(s.steps[self.step as usize].kind, Kind::Account)
    }

    #[inline]
    fn navblur_on(&self) -> bool {
        self.navblur > 0
    }

    /// Is anything in this module armed? Nothing below runs — and no heartbeat field appears —
    /// unless this is true.
    #[inline]
    pub(crate) fn armed(&self) -> bool {
        self.sweep.is_some() || self.navblur_on()
    }
}

/// The live step index, or -1. Rides the heartbeat and every profiler record — read from the
/// published snapshot, since neither caller holds the plan.
#[inline]
pub(crate) fn step_index() -> i32 {
    STEP_PUB.load(std::sync::atomic::Ordering::Relaxed)
}

/// Is anything in this module armed? The published half of [`Dial::armed`], for the same readers.
#[inline]
pub(crate) fn armed() -> bool {
    ARMED_PUB.load(std::sync::atomic::Ordering::Relaxed)
}

/// Advance the dial one presented frame, BEFORE the page draws.
///
/// Two things happen here and both have to precede every glass surface in the frame: the step may
/// roll over (which invalidates, so a configuration change is not measured against the last one's
/// cached snapshot), and the cadence clock may invalidate. `Glass::prepare`'s contract, for the
/// same reason.
impl Dial {
    pub(crate) fn prepare(&mut self, now_ms: u32) {
        if !self.armed() {
            return;
        }
        // A dial run is a perf measurement: it must keep presenting whatever the idle gate thinks.
        // `wake` rather than `invalidate` — nothing here claims the PAGE's pixels changed.
        crate::ui::idle::wake();
        let Some((hold_ms, len)) = self.sweep.as_ref().map(|s| (s.hold_ms, s.steps.len())) else {
            return;
        };
        if self.t0 == 0 {
            self.t0 = now_ms.max(1);
        }
        let idx = ((now_ms.saturating_sub(self.t0) / hold_ms) as usize % len) as i32;
        if idx != self.step {
            self.step = idx;
            self.presents = 0;
            self.publish();
            crate::gfx::blur_invalidate();
            crate::log(&format!("GLASSLOAD step={idx}"));
            return;
        }
        self.presents = self.presents.wrapping_add(1);
        let cad = self
            .sweep
            .as_ref()
            .map_or(0, |s| s.steps[idx as usize].cadence);
        if cad > 0 && self.presents % cad == 0 {
            crate::gfx::blur_invalidate();
        }
    }

    /// Draw the dial's glass surfaces over whatever screen is up. Called after the route's own draw,
    /// so the snapshot these take is of the COMPLETE page — which is the honest source for a surface
    /// that sits on top of everything.
    pub(crate) fn draw(&mut self) {
        if self.sweep.is_none() {
            return;
        }
        // A surface met while the page is being drawn as a blur SOURCE must not draw, record a need
        // or take a capture — `gfx::draw_blur_backdrop` refuses anyway, but the frost quad would
        // still land in the source target. See `widgets::tab_glass_on`.
        if crate::gfx::blur_source_pass() {
            return;
        }
        let idx = self.step;
        if idx < 0 {
            return;
        }
        let p = Painter::root();
        // Copied out before the loop: the surfaces are drawn from `&mut self` (the card textures
        // are uploaded on first use), so the sweep may not stay borrowed across them.
        let Some(step) = self
            .sweep
            .as_ref()
            .and_then(|s| s.steps.get(idx as usize).copied())
        else {
            return;
        };
        for (i, r) in layout(step).into_iter().enumerate() {
            match step.kind {
                Kind::Glass => {
                    if p.backdrop_blur(
                        r,
                        0.0,
                        PANEL_RADIUS,
                        [1.0, 1.0, 1.0, 1.0],
                        crate::gfx::GlassRim::Bevelled,
                        crate::gfx::GlassFace::NONE,
                        theme::Material::Regular.deep(),
                    ) {
                        crate::ui::profile::phase("glass.frost", || {
                            p.rect(
                                r,
                                PANEL_RADIUS,
                                theme::PANEL_FROST_TOP,
                                theme::PANEL_FROST_BOT,
                                0.0,
                            );
                        });
                    } else {
                        p.rect(r, PANEL_RADIUS, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
                    }
                }
                Kind::Card { focused } => {
                    let f = if focused { 1.0 } else { 0.0 };
                    p.tex_carded(
                        self.card_tex(i),
                        r,
                        theme::CARD_RING_RAD,
                        [1.0, 1.0, 1.0, 1.0],
                        f,
                    );
                }
                Kind::Image => p.tex(self.card_tex(i), r, 0.0, [1.0, 1.0, 1.0, 1.0]),
                // The real popover is drawn by its own surface on the container tree; `layout` returns no
                // rects for it, so this arm is unreachable and says so rather than drawing a stand-in.
                Kind::Account => unreachable!("an Account step lays out no surfaces of its own"),
            }
        }
    }

    /// The synthetic poster textures the card steps sample, allocated once and reused for the process
    /// lifetime (never per frame — see the shared brief's constraint).
    ///
    /// **Four of them, in rotation, and that is the load-bearing part.** A grid of N cards all reading
    /// ONE texture is a texture cache that stays warm and a memory system that never has to work, which
    /// would price a poster wall as cheaper than it is. Four 512x768 RGBA images are 6 MB, comfortably
    /// past any on-chip cache on this part, so consecutive cards miss the way real posters do. They are
    /// value noise rather than a flat fill for the same reason: a constant texture compresses in the
    /// framebuffer/L2 path and a photograph does not.
    fn card_tex(&mut self, i: usize) -> u32 {
        const W: usize = 512;
        const H: usize = 768;
        let slot = i % NCARD_TEX;
        let tex = self.card_tex[slot];
        if tex != 0 {
            return tex;
        }
        let mut px = vec![0u8; W * H * 4];
        // xorshift, so the bytes are incompressible and the image is identical on every run —
        // a measurement whose texture content varied would not be re-runnable.
        let mut state: u32 = 0x9E37_79B9 ^ ((slot as u32 + 1) * 0x85EB_CA6B);
        for byte in px.iter_mut() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        let id = crate::gfx::upload_rgba(0, W as i32, H as i32, px.as_ptr());
        self.card_tex[slot] = id;
        id
    }
}

/// The full-bleed glass slab the transition prototype cross-fades in.
///
/// Inflated well past the screen on every side ON PURPOSE: `fs_glass` puts its bevel, rim light and
/// specular hairline within `GLASS_BEVEL` of the edge, and a full-screen blur must not be ringed by
/// a lit chamfer. Pushing the geometry off-panel leaves only the interior early-out — one texture
/// fetch plus dither per fragment — which is exactly what a full-screen backdrop blur is.
const NAV_BLEED: f32 = 80.0;

/// The tab track's rest geometry, as a stand-in for the real strip.
///
/// The real one is `widgets::draw_tab_row`'s fixed four-destination strip, drawn INSIDE the page —
/// i.e. underneath anything this module composites. A representative capsule at its true y and
/// height is what makes "the bar's glass
/// on top of the blur" a thing that can be both measured and looked at.
fn nav_capsule() -> Rect {
    let w = 1140.0;
    let h = crate::ui::widgets::TAB_PILL_H + 2.0 * crate::ui::widgets::TAB_TRACK_PAD;
    Rect::new(
        (SCR_W - w) * 0.5,
        crate::ui::widgets::TOP_BAR_Y - crate::ui::widgets::TAB_TRACK_PAD,
        w,
        h,
    )
}

impl Dial {
    /// Draw the blurred route transition, if one is in flight. Returns whether it drew.
    pub(crate) fn draw_nav_blur(&mut self) -> bool {
        if !self.navblur_on() || crate::gfx::blur_source_pass() {
            return false;
        }
        let amount = if self.navblur_pin {
            1.0
        } else {
            crate::ui::nav::blur_amount()
        };
        if amount <= 0.002 {
            return false;
        }
        // The cadence clock for this surface is the transition's own: a snapshot per `navblur_cadence`
        // presented frames while the fade is running. Counted here rather than in `prepare` because
        // the fade is the only thing that makes the backdrop stale.
        self.presents = self.presents.wrapping_add(1);
        if self.navblur_cadence > 0 && self.presents % self.navblur_cadence == 0 {
            crate::gfx::blur_invalidate();
        }
        let p = Painter::root();
        let full = Rect::new(
            -NAV_BLEED,
            -NAV_BLEED,
            SCR_W + 2.0 * NAV_BLEED,
            SCR_H + 2.0 * NAV_BLEED,
        );
        let drew = p.backdrop_blur(
            full,
            0.0,
            0.0,
            [1.0, 1.0, 1.0, amount],
            crate::gfx::GlassRim::Bevelled,
            crate::gfx::GlassFace::NONE,
            theme::Material::Regular.deep(),
        );
        // Mode 2: a PRIVATE cache for the surface above. There is one snapshot chain in this renderer,
        // so the only way to give the capsule a backdrop that includes the slab beneath it is to take
        // the whole chain again — which is precisely the cost a second cache would have.
        if self.navblur >= 2 {
            crate::gfx::blur_invalidate();
        }
        let cap = nav_capsule();
        if p.backdrop_blur(
            cap,
            0.0,
            cap.h * 0.5,
            [1.0, 1.0, 1.0, amount],
            crate::gfx::GlassRim::Standing,
            crate::gfx::GlassFace::NONE,
            theme::Material::Regular.deep(),
        ) {
            p.alpha(amount).rect_sheened(
                cap,
                cap.h * 0.5,
                theme::TAB_GLASS_TOP,
                theme::TAB_GLASS_BOT,
            );
        }
        drew
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The dial is a measuring instrument, so what these grade is that it measures the
    //! configuration it was asked for: a mis-parsed step or a silently dropped panel would report
    //! a cost for a scene that never ran, which is the one failure a device run cannot catch.
    use super::*;

    #[test]
    fn a_spec_parses_into_the_steps_it_names() {
        let s = parse("hold=6;off,1x608x396@3,2x400x300@1").expect("spec");
        assert_eq!(s.hold_ms, 6000);
        assert_eq!(s.steps.len(), 3);
        assert_eq!(s.steps[0], Step::OFF);
        let g = |n, w, h, cadence| Step {
            kind: Kind::Glass,
            spread: false,
            n,
            w,
            h,
            cadence,
        };
        assert_eq!(s.steps[1], g(1, 608.0, 396.0, 3));
        assert_eq!(s.steps[2], g(2, 400.0, 300.0, 1));
    }

    #[test]
    fn the_cadence_defaults_to_the_shipped_policy_and_the_hold_to_six_seconds() {
        let s = parse("1x608x396").expect("spec");
        assert_eq!(
            s.hold_ms, 6000,
            "a step shorter than a few heartbeats grades nothing"
        );
        assert_eq!(
            s.steps[0].cadence, 1,
            "the shipped dynamic glass refreshes every changed present"
        );
    }

    #[test]
    fn nonsense_disarms_the_dial_rather_than_half_arming_it() {
        for bad in [
            "",
            "1x608",
            "zx1x1",
            "1x608x396x2",
            "0x608x396",
            "hold=0.1;1x608x396",
            "hold=6",
        ] {
            assert!(parse(bad).is_none(), "{bad:?} must not arm a partial sweep");
        }
    }

    #[test]
    fn a_panel_larger_than_the_screen_is_clamped_to_it() {
        let s = parse("1x4000x4000").expect("spec");
        assert_eq!(s.steps[0].w, SCR_W);
        assert_eq!(s.steps[0].h, SCR_H);
        let r = layout(s.steps[0]);
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].x, r[0].y, r[0].w, r[0].h), (0.0, 0.0, SCR_W, SCR_H));
    }

    #[test]
    fn panels_tile_inside_the_screen_and_the_count_is_what_actually_fits() {
        // Four 608x396 panels fit two across and two down on a 1920x1080 canvas.
        let four = layout(Step {
            kind: Kind::Glass,
            spread: false,
            n: 4,
            w: 608.0,
            h: 396.0,
            cadence: 3,
        });
        assert_eq!(four.len(), 4);
        assert!(four.iter().all(|r| r.x >= 0.0 && r.y >= 0.0));
        assert!(four
            .iter()
            .all(|r| r.x + r.w <= SCR_W + 0.01 && r.y + r.h <= SCR_H + 0.01));
        // …and asking for more than fits reports the number DRAWN, never the number requested.
        let many = layout(Step {
            kind: Kind::Glass,
            spread: false,
            n: 99,
            w: 608.0,
            h: 396.0,
            cadence: 3,
        });
        assert!(
            many.len() < 99 && !many.is_empty(),
            "packed {} of 99",
            many.len()
        );
        assert!(many
            .iter()
            .all(|r| r.x + r.w <= SCR_W + 0.01 && r.y + r.h <= SCR_H + 0.01));
    }

    #[test]
    fn the_control_leg_draws_nothing_at_all() {
        assert!(layout(Step::OFF).is_empty());
    }

    /// The card axis: `c`/`cf` must select the card composite, and `cf` must not be read as an
    /// ordinary step whose count starts with `f` — the longer prefix has to be tried first, and
    /// getting that wrong rejects the whole spec rather than one token.
    #[test]
    fn a_card_step_is_the_same_geometry_down_a_different_shader() {
        let s = parse("hold=6;off,c40x220x330,cf40x220x330,1x608x396@3").expect("spec");
        assert_eq!(s.steps[1].kind, Kind::Card { focused: false });
        assert_eq!(
            parse("i1x960x540").expect("spec").steps[0].kind,
            Kind::Image
        );
        assert_eq!(s.steps[2].kind, Kind::Card { focused: true });
        assert_eq!(s.steps[3].kind, Kind::Glass);
        assert_eq!(s.steps[1].n, 40);
        assert_eq!(s.steps[1].px(), 40.0 * 220.0 * 330.0);
        // …and the two card steps lay out identically, so a focused/resting A/B changes ONE thing.
        // `Rect` is neither `Debug` nor `PartialEq`, so compare the fields the layout decides.
        let (a, b) = (layout(s.steps[1]), layout(s.steps[2]));
        assert_eq!(a.len(), b.len());
        assert!(a
            .iter()
            .zip(&b)
            .all(|(p, q)| p.x == q.x && p.y == q.y && p.w == q.w && p.h == q.h));
    }

    /// The step that puts the SHIPPED surface on the same dial as the synthetic ones. It draws
    /// nothing itself — `app.rs` routes to the real popover — so laying out no rects is the
    /// contract, not an oversight.
    #[test]
    fn the_account_step_asks_for_a_route_rather_than_drawing_a_stand_in() {
        let s = parse("hold=6;off,acct,1x440x220@3").expect("spec");
        assert_eq!(s.steps[1].kind, Kind::Account);
        assert!(
            layout(s.steps[1]).is_empty(),
            "the real popover draws itself, on its own route"
        );
        assert_eq!(s.steps[2].kind, Kind::Glass);
    }

    /// Spread is what makes the one shared blur region span the whole canvas: two surfaces in
    /// opposite corners cover the same pixels as two adjacent ones and cost a region that holds
    /// everything between them as well.
    #[test]
    fn a_spread_step_puts_its_surfaces_in_opposite_corners() {
        let s = parse("s2x300x300@3").expect("spec");
        assert!(s.steps[0].spread);
        let r = layout(s.steps[0]);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].x, r[0].y), (0.0, 0.0));
        assert_eq!((r[1].x, r[1].y), (SCR_W - 300.0, SCR_H - 300.0));
        // …and the same spec without the `s` keeps them together in the middle.
        let t = parse("2x300x300@3").expect("spec");
        let a = layout(t.steps[0]);
        assert!(
            a[0].x > 300.0 && a[1].x < SCR_W - 300.0,
            "adjacent, not cornered"
        );
    }

    #[test]
    fn the_transition_spec_names_a_mode_a_cadence_and_whether_it_is_pinned() {
        assert_eq!(
            parse_navblur(""),
            Some((1, 1, false)),
            "empty is the shipped-cadence default"
        );
        assert_eq!(parse_navblur("1"), Some((1, 1, false)));
        assert_eq!(
            parse_navblur("2:1"),
            Some((2, 1, false)),
            "a private second cache, every frame"
        );
        assert_eq!(
            parse_navblur("1p"),
            Some((1, 1, true)),
            "pinned for a capture"
        );
        assert_eq!(parse_navblur("2p:6"), Some((2, 6, true)));
        for bad in ["3", "0", "x", "1:x", "9p"] {
            assert!(
                parse_navblur(bad).is_none(),
                "{bad:?} must not arm a mode nobody asked for"
            );
        }
    }

    #[test]
    fn the_transition_capsule_sits_on_the_real_tab_track_line() {
        let c = nav_capsule();
        assert_eq!(
            c.y,
            crate::ui::widgets::TOP_BAR_Y - crate::ui::widgets::TAB_TRACK_PAD
        );
        assert_eq!(
            c.h,
            crate::ui::widgets::TAB_PILL_H + 2.0 * crate::ui::widgets::TAB_TRACK_PAD
        );
        assert!(
            c.x > 0.0 && c.x + c.w < SCR_W,
            "the strip is inset from both edges"
        );
    }
}
