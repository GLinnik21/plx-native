//! The popover panel's GLASS — the look every menu shares (design boards `WatchlistOptions`,
//! `HoldMenu`, `WatchlistHold`): a gradient face over the page the menu stands on, a 1 px rim, a
//! top highlight on the rim, and a soft shadow. The 30% scrim behind it is the compact-menu dim
//! role (`theme::underlay::DIM_COMPACT`), not a number stated here.
//!
//! **One entry point.** [`draw`] is called by [`widgets::panel_ground`](crate::widgets::panel_ground)
//! for a [`Ground::Menu`](crate::widgets::Ground) only (menus, not sheets or alerts), so no screen
//! restates any of this. It reports `false` — and the caller draws the latched-underlay ground it
//! always did — when there is nothing frozen to stand on: the field is not latched yet (a unit test
//! with no frame, the first frame of a modal), a surface over the hardware video plane, or a menu
//! stacked over another surface's dim.
//!
//! **The glass stands on FROZEN ground and reads no framebuffer** (owner decision, 2026-10-17). A
//! menu stands on a page that is not moving (`popover.rs`'s module doc), so its glass is fed the two
//! pictures of that page the frame already holds, through `gfx::draw_glass_frozen`: the body shows
//! the latched underlay field (`ui::underlay`, the 60x32 reconstruction the modal dim draws), and
//! the rim's lens bends the host snapshot (`popover::host::page_tex`, the one capture of the
//! undimmed page) at `tokens::MENU_SHARP` — `fs_glass.frag`'s refraction with the SAME rounded-rect
//! SDF it rounds the panel with. Both are undimmed, and the frame's one modal dim ([`note_dim`]) is
//! applied to both at composite. No backdrop entry is requested, no blur source pass runs and no
//! band is copied off the framebuffer, so the menu's glass costs one panel quad (and its shadow) on every frame of
//! its open, its stay and its close, and a morph or a slide moves that quad over pictures that do
//! not change. It replaced a live `Glass::DYNAMIC_BACKDROP` surface that re-ran the page at quarter
//! resolution as its blur source whenever the draws beneath it changed (the `src` and `pagex2`
//! spans) and composited a 5-tap blur plus a band copy every frame: about 2 to 2.6M Mali GPU
//! cycles a frame over the field ground (television, 2026-10-17).
//!
//! **What it does not see**: anything drawn live above the host after its capture — an opener
//! lifted over the dim (the item menu's card, the account chip) — is not in the snapshot, so the
//! rim refracts the page as captured without it, which is also why the Sort & Filter pill (lifted
//! out of the page, `Screen::opener`) leaves no ghost in the lens.
//!
//! **Board against shipped code** (shipped code wins, recorded): the board's panel radius is 26 and
//! the design system's `--radius-panel` is 24, while the shipped panels state their own
//! (`theme::ALERT_PANEL_RAD` 32 and the menus' `PANEL_RAD`) — the radius is the caller's, as it
//! always was. The board's top highlight is white .24; the shipped `theme::GLASS_RIM_LIGHT` is .28
//! and stays. The board's `saturate(1.6)` has no counterpart in the composite and is not drawn.
use crate::frame::backdrop::Dim;
use crate::{theme, Painter, Rect};
use plx_gfx::gfx::{GlassFace, GlassRim};

/// The face gradient's top stop: [`theme::MENU_GLASS_TOP`], the board's `rgba(58,58,64,.78)`.
pub const FACE_TOP: [f32; 4] = theme::MENU_GLASS_TOP;
/// The face gradient's bottom stop: [`theme::MENU_GLASS_BOT`], the board's `rgba(40,40,46,.86)`.
pub const FACE_BOT: [f32; 4] = theme::MENU_GLASS_BOT;

/// What the glass wears over its backdrop at full strength: the gradient face, the 1 px perimeter
/// and the lit top edge — the board's `inset 0 0 0 1px` and `inset 0 1px 0`, as the shader draws
/// them in the surface's own coverage.
pub const FACE: GlassFace = GlassFace {
    scrim_top: FACE_TOP,
    scrim_bot: FACE_BOT,
    rim: theme::GLASS_RIM,
    rim_lit: theme::GLASS_RIM_LIGHT,
    rim_w: theme::CARD_SHEEN_W,
};

/// The face at opacity `a`. Only the tint rides the painter's alpha cascade; a [`GlassFace`] does
/// not, so a panel that fades (appear, close, a route change's page dip) fades its face by hand
/// the way the hold hint does — otherwise a ~.8-opaque gradient and a full rim stand over rows
/// that are still near 0, which is the empty glass frame the morph forbids.
pub(crate) fn face_at(a: f32) -> GlassFace {
    crate::widgets::chip_face(FACE, a.clamp(0.0, 1.0))
}

thread_local! {
    /// The modal dims drawn on this frame's visible pass so far, each with the index of the surface
    /// that owns it: `Painter::dim` notes each ([`note_dim`]), `popover::host::begin_frame` and
    /// `app/run.rs`'s frame head clear. Per thread, like the walk.
    static DIMS: std::cell::RefCell<Vec<(usize, Dim)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The surface whose dim is being painted ([`dim_owner`]); `note_dim` stamps it on the dim.
    static DIM_OWNER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The surface being drawn ([`drawing_surface`]); `usize::MAX` outside the surface walk, where
    /// every dim noted so far counts.
    static SURFACE: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// A modal dim was just drawn on the visible frame, under whatever surface draws next. The glass's
/// two pictures are undimmed, so it applies this at composite.
pub(crate) fn note_dim(d: Dim) {
    let owner = DIM_OWNER.with(std::cell::Cell::get);
    DIMS.with(|c| c.borrow_mut().push((owner, d)));
}

/// Run `f` (a dim's paint) as the dim of surface `index` (its position in the stack, bottom = 0).
/// `ModalStack::draw_scrims_on` records EVERY surface's dim before any surface draws, so the
/// frame's count alone cannot say how many lie below a given surface.
pub(crate) fn dim_owner<R>(index: usize, f: impl FnOnce() -> R) -> R {
    let prev = DIM_OWNER.with(|c| c.replace(index));
    let r = f();
    DIM_OWNER.with(|c| c.set(prev));
    r
}

/// Run `f` (one surface's draw) as surface `index`: its glass counts only the dims at or below it.
pub(crate) fn drawing_surface<R>(index: usize, f: impl FnOnce() -> R) -> R {
    let prev = SURFACE.with(|c| c.replace(index));
    let r = f();
    SURFACE.with(|c| c.set(prev));
    r
}

/// A new frame: no dim drawn yet. Called at the head of EVERY frame (`app/run.rs`, whatever the
/// route) and by `popover::host::begin_frame`, before anything draws.
pub(crate) fn clear_dims() {
    DIMS.with(|c| c.borrow_mut().clear());
}

/// How many dims the current surface stands under, and the last of them.
fn dims_under_surface() -> (usize, Option<Dim>) {
    let own = SURFACE.with(std::cell::Cell::get);
    DIMS.with(|c| {
        let c = c.borrow();
        let mut under = c.iter().filter(|(owner, _)| *owner <= own);
        let n = under.clone().count();
        (n, under.next_back().map(|(_, d)| *d))
    })
}

/// Draw the glass panel at `r` with corner `radius`, at the opacity the painter carries
/// ([`Painter::opacity`]), standing on `field` (the latched underlay field, `DrawFrame::underlay`).
/// `false` = nothing frozen to stand on, nothing drawn.
#[must_use]
pub fn draw(p: Painter, r: Rect, radius: f32, field: Option<&crate::underlay::UnderlayField>) -> bool {
    draw_shadowed(p, r, radius, field, 1.0)
}

/// [`draw`] with the drop shadow at `shadow` of the painter's opacity: a morph hands the glass and
/// its shadow the same fill ramp (`Popover::ground`), a fallback open keeps its shadow whole.
#[must_use]
pub fn draw_shadowed(
    p: Painter,
    r: Rect,
    radius: f32,
    field: Option<&crate::underlay::UnderlayField>,
    shadow: f32,
) -> bool {
    let Some(ground) = field.and_then(crate::underlay::UnderlayField::ground_tex) else {
        return false;
    };
    // The host snapshot is the page under the FIRST surface's dim. A menu over another surface
    // stands on that surface too, which neither picture holds: it keeps the field ground. Only the
    // dims at or BELOW this surface count: the whole frame's dims are recorded before any surface
    // draws, so a closing lower menu must not read the dim of a surface opened above it.
    let (dims, dim) = dims_under_surface();
    if dims > 1 {
        return false;
    }
    let sharp = crate::popover::host::page_tex().unwrap_or(0);
    let face = face_at(p.opacity());
    if !p.glass_frozen(r, radius, ground, sharp, dim, [1.0; 4], GlassRim::Menu, face) {
        return false;
    }
    // After the glass, not before: `shadow_outside` cuts the panel's own rounded shape out of the
    // penumbra, so a translucent face has no ink under it. The shadow rides the painter's cascade
    // (alpha and slide) itself.
    if shadow > 0.0 {
        p.alpha(shadow).shadow_outside(r, radius, theme::MENU_SHADOW_BLUR, theme::MENU_SHADOW_DY, theme::MENU_SHADOW);
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn the_face_is_the_boards_gradient() {
        assert_eq!(FACE_TOP[3], 0.78);
        assert_eq!(FACE_BOT[3], 0.86);
        assert!(FACE_TOP[0] > FACE_BOT[0], "lighter at the top, as the board's gradient runs");
    }

    #[test]
    fn the_menu_shadow_blur_is_30px_for_every_glass_menu() {
        assert_eq!(theme::MENU_SHADOW_BLUR, 30.0);
    }

    #[test]
    fn the_rim_and_highlight_are_the_shipped_glass_tokens() {
        assert_eq!(FACE.rim, theme::GLASS_RIM);
        assert_eq!(FACE.rim_lit, theme::GLASS_RIM_LIGHT);
        assert_eq!(FACE.rim_w, 1.0, "the board's 1 px");
    }

    /// Every paint `f` declares inside a backdrop DISCOVERY pass, in order.
    pub(crate) fn declared_all(f: impl FnOnce()) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
        use crate::frame::backdrop::{discover, Sources};
        use std::{cell::RefCell, rc::Rc};
        let sources = Rc::new(RefCell::new(Sources::default()));
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            f();
        }
        let paints = sources.borrow().paints.clone();
        paints
    }

    /// The menu glasses `f` declares inside a discovery pass, in order.
    pub(crate) fn declared_glass(f: impl FnOnce()) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
        declared_all(f).into_iter().filter(|p| p.kind() == "glass_frozen").collect()
    }

    /// A field latched on a stand-in texture, as `ModalUnderlay` holds one once its read lands.
    /// Drop it with [`unlatch`].
    pub(crate) fn latched(tex: u32) -> crate::underlay::UnderlayField {
        plx_gfx::gfx::tex_ledger::specified(tex, 60, 32);
        crate::underlay::UnderlayField::latched_for_test(tex)
    }
    pub(crate) fn unlatch(tex: u32) {
        plx_gfx::gfx::tex_ledger::deleted(tex);
    }

    /// The glass the board's panel should declare at `r` over `field`, built straight from the
    /// pieces (no `draw`): the frozen composite, the menu rim, the face at opacity `face`.
    pub(crate) fn expected(
        p: Painter,
        r: Rect,
        rad: f32,
        ground: u32,
        sharp: u32,
        face: GlassFace,
    ) -> Vec<std::rc::Rc<crate::frame::backdrop::Paint>> {
        declared_glass(|| {
            assert!(p.glass_frozen(r, rad, ground, sharp, None, [1.0; 4], GlassRim::Menu, face));
        })
    }

    /// The face fades with the panel, through `draw` itself: the declared glass at opacity `a`
    /// equals the one built from `face_at(a)` and differs from the unfaded `FACE` one. Red if
    /// `draw` hands `FACE` as is (the finding: a ~.8-opaque face over rows still near 0).
    #[test]
    fn draw_declares_the_face_at_the_panels_opacity() {
        let _g = plx_base::testlock::serial();
        let tex = 9_741;
        let field = latched(tex);
        let r = Rect::new(100.0, 100.0, 300.0, 200.0);
        for a in [0.1_f32, 0.5] {
            let p = Painter::root().alpha(a);
            let drawn = declared_glass(|| assert!(draw(p, r, 24.0, Some(&field)), "a latched field takes the glass"));
            assert_eq!(drawn.len(), 1, "one glass per panel");
            assert!(drawn[0] == expected(p, r, 24.0, tex, 0, face_at(a))[0], "the face rides the opacity {a}");
            assert!(drawn[0] != expected(p, r, 24.0, tex, 0, FACE)[0], "an unfaded face is a different draw");
        }
        let p = Painter::root();
        let full = declared_glass(|| assert!(draw(p, r, 24.0, Some(&field))));
        assert!(full[0] == expected(p, r, 24.0, tex, 0, FACE)[0], "at full opacity the face is FACE");
        unlatch(tex);
    }

    /// **Nothing frozen to stand on, no glass**: no field, or one not latched yet (the first frames
    /// of a modal, before its read lands), or a menu stacked over another surface's dim (neither
    /// picture holds that surface). The caller then draws the field ground it always did.
    #[test]
    fn without_frozen_ground_the_glass_draws_nothing() {
        let _g = plx_base::testlock::serial();
        let r = Rect::new(100.0, 100.0, 300.0, 200.0);
        let p = Painter::root();
        assert!(declared_glass(|| assert!(!draw(p, r, 24.0, None))).is_empty());
        let unlatched = crate::underlay::UnderlayField::new();
        assert!(declared_glass(|| assert!(!draw(p, r, 24.0, Some(&unlatched)))).is_empty());
        let tex = 9_742;
        let field = latched(tex);
        let ink = Dim { tex, tint: [0.0, 0.0, 0.0, 0.3] };
        clear_dims();
        note_dim(ink);
        assert!(!declared_glass(|| assert!(draw(p, r, 24.0, Some(&field)))).is_empty(), "one dim: the menu's own");
        note_dim(ink);
        assert!(declared_glass(|| assert!(!draw(p, r, 24.0, Some(&field)))).is_empty(), "a second surface's dim");
        clear_dims();
        unlatch(tex);
    }

    /// **Every surface's dim is recorded before any surface draws** (`draw_scrims_on`), so a closing
    /// lower menu drawn in a frame that also holds a newer surface's dim must still count ONE dim:
    /// it keeps its frozen glass instead of flipping to the plain field mid-fade. The upper surface
    /// stands on the lower one, which neither picture holds, and keeps the fallback.
    #[test]
    fn a_lower_menu_counts_only_the_dims_below_its_own_surface() {
        let _g = plx_base::testlock::serial();
        let r = Rect::new(100.0, 100.0, 300.0, 200.0);
        let p = Painter::root();
        let tex = 9_744;
        let field = latched(tex);
        let ink = Dim { tex, tint: [0.0, 0.0, 0.0, 0.3] };
        clear_dims();
        dim_owner(0, || note_dim(ink));
        dim_owner(1, || note_dim(ink));
        let lower = declared_glass(|| drawing_surface(0, || assert!(draw(p, r, 24.0, Some(&field)))));
        assert!(!lower.is_empty(), "the lower, closing menu keeps frozen glass");
        let upper = declared_glass(|| drawing_surface(1, || assert!(!draw(p, r, 24.0, Some(&field)))));
        assert!(upper.is_empty(), "the stacked upper menu keeps the field ground");
        clear_dims();
        unlatch(tex);
    }

    /// One frame of a menu over a page and its modal dim, the order `dispatch.rs::draw_with` walks
    /// them in: the page (`page`, a colour that changes when the page re-renders), the dim at
    /// `alpha` (`ModalStack::draw_scrims` → `UnderlayField::draw` as `Role::Dim`), then the menu
    /// in its surface band, drawn by `menu`. Returns whether ANY backdrop entry or capture job
    /// exists. (Nothing in the surfaces band is recorded, so the glass itself is read off a
    /// separate discovery pass, [`declared_glass`].)
    fn menu_frame(
        sources: &std::rc::Rc<std::cell::RefCell<crate::frame::backdrop::Sources>>,
        field: &crate::underlay::UnderlayField,
        page: f32,
        alpha: f32,
        menu: impl FnOnce(Painter),
    ) -> bool {
        use crate::frame::backdrop::{commit, discover, layer, Z};
        sources.borrow_mut().begin(vec![]);
        {
            let _walk = discover(sources.clone());
            let p = Painter::root();
            p.rect(Rect::FULL, 0.0, [page, 0.3, 0.4, 1.0], [page, 0.3, 0.4, 1.0], 0.0);
            {
                let _dim = layer(Z::DIM, false);
                field.draw(p, Rect::FULL, crate::underlay::Role::Dim { weight: theme::underlay::TINT }, alpha);
            }
            let _surface = layer(Z::surface(0), false);
            menu(p);
        }
        let live = {
            let mut s = sources.borrow_mut();
            s.resolve();
            !s.entries.is_empty() || !s.jobs().is_empty()
        };
        commit(sources);
        live
    }

    /// **A menu's glass asks the backdrop for nothing, through its whole life.** Over every frame
    /// of an open, a stay and a close — with the page under it re-rendering on each frame and the
    /// dim ramping with the appear — the walk holds no backdrop entry and owes no capture job, so
    /// no blur source pass (`src`), no second page draw (`pagex2`) and no band copy can run for it.
    /// The morph (Sort & Filter, the account menu) and the slide (the item hold menu) both. Red
    /// on the build whose menu glass was a `Glass::DYNAMIC_BACKDROP` surface: its entry's source
    /// was the page and the dim, and every frame either of them changed was a job (the test it
    /// replaces, `a_menu_glass_inside_a_surface_resolves_against_the_page_and_the_scrim`, asserted
    /// exactly that entry).
    #[test]
    fn a_menu_glass_requests_no_backdrop_entry_while_opening_open_or_closing() {
        use crate::popover::Popover;
        let _g = plx_base::testlock::serial();
        let tex = 9_743;
        let field = latched(tex);
        let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
        let pill = Rect::new(1700.0, 150.0, 180.0, 56.0);
        let panel = Rect::new(1420.0, 220.0, 472.0, 600.0);
        for morph in [true, false] {
            let mut pop = if morph { Popover::new().morphing() } else { Popover::new() };
            pop.open_from(Some(pill));
            let (mut frames, mut painted, mut closing) = (0, 0, false);
            while pop.visible() {
                pop.update(1.0 / 60.0);
                let a = pop.appear();
                let menu = |p: Painter| pop.panel(pop.content_painter_on(p, Popover::RISE), panel, 24.0, Some(&field), crate::widgets::Ground::Menu);
                let live = menu_frame(&sources, &field, (frames % 7) as f32 * 0.1, theme::underlay::DIM_COMPACT * a, menu);
                let glass = declared_glass(|| menu(Painter::root()));
                assert!(!live, "frame {frames} (morph {morph}, closing {closing}): no backdrop entry, no job");
                assert!(glass.len() <= 1);
                painted += glass.len();
                frames += 1;
                if !closing && pop.appear_settled() {
                    // The stay: the page keeps re-rendering under the settled menu.
                    for i in 0..10 {
                        let live = menu_frame(&sources, &field, i as f32 * 0.1, theme::underlay::DIM_COMPACT, menu);
                        assert!(!live, "settled frame {i}: the glass stands, asking for nothing");
                        assert_eq!(declared_glass(|| menu(Painter::root())).len(), 1, "settled frame {i}: the glass stands");
                    }
                    let _input = crate::popover::host::input_scope();
                    pop.dismiss();
                    closing = true;
                }
                assert!(frames < 600, "the menu opens and closes");
            }
            assert!(closing && painted > 8, "the glass drew over the open and the close (morph {morph}): {painted}");
            sources.borrow_mut().clear();
        }
        unlatch(tex);
    }

    /// **The profile chip's opener lift stands on frozen ground too.** The account menu lifts the
    /// chip back out of its dim (`widgets::lift_profile_chip`, the lift pass's); that capsule was a live
    /// `Glass::DYNAMIC_BACKDROP` whose source was the re-rendered page (the `src` job, three
    /// frames per open and per close). With the stack's latched field handed through
    /// `Rig::lift_bar_member`'s ground it requests no backdrop entry and no job, on every frame of an open,
    /// a stay and a close, and declares one `glass_frozen` for the capsule. Red on the live lift.
    #[test]
    fn the_chip_lift_stands_on_frozen_ground_and_asks_for_nothing() {
        use crate::frame::backdrop::{commit, discover, layer, Sources, Z};
        let _g = plx_base::testlock::serial();
        let tex = 9_745;
        let field = latched(tex);
        let sources = std::rc::Rc::new(std::cell::RefCell::new(Sources::default()));
        let profile = crate::widgets::ProfileChipRead { src: 0, thumb: "", initial: c"G", name: c"Guest", name_w: 60.0 };
        let face = crate::widgets::chip_face(FACE, 1.0);
        let lift = |ground: &crate::underlay::UnderlayField| layer_dim(|| {
            crate::widgets::lift_profile_chip(Painter::root(), profile, 0.0, Some(face), ground)
        });
        fn layer_dim(f: impl FnOnce()) {
            let _dim = layer(Z::DIM, false);
            f();
        }
        // open (ramping dim), stay (page re-rendering), close (dim ramping down)
        for (i, alpha) in [0.05_f32, 0.15, 0.3, 0.3, 0.3, 0.2, 0.1, 0.0].into_iter().enumerate() {
            sources.borrow_mut().begin(vec![]);
            {
                let _walk = discover(sources.clone());
                let p = Painter::root();
                p.rect(Rect::FULL, 0.0, [(i % 3) as f32 * 0.2, 0.3, 0.4, 1.0], [0.1, 0.3, 0.4, 1.0], 0.0);
                {
                    let _dim = layer(Z::DIM, false);
                    field.draw(p, Rect::FULL, crate::underlay::Role::Dim { weight: theme::underlay::TINT }, alpha);
                }
                lift(&field);
            }
            let live = {
                let mut s = sources.borrow_mut();
                s.resolve();
                !s.entries.is_empty() || !s.jobs().is_empty()
            };
            commit(&sources);
            assert!(!live, "frame {i}: the chip lift asks for no backdrop entry and no job");
            let glass = declared_glass(|| lift(&field));
            assert_eq!(glass.len(), 1, "frame {i}: the capsule is one frozen glass");
        }
        unlatch(tex);
    }

    /// **A lift whose ground has not latched draws NOTHING** (CONFIRM2 CB1). The field latches a frame
    /// after the OK that opens the menu (`gfx::FIELD_READ_LAG_SWAPS`), and on that frame the lift used
    /// to fall back to the flat `scrim_black(TAB_TRACK_A)` capsule over the capsule the bar had already
    /// drawn UNDER it: the chip blinked near-black for one frame on every open (F3 `c/chip-open.png`
    /// n=194), because the dim is 0 on that frame and nothing darkens the bar's own chip but the second
    /// capsule. The bar's chip is on screen already; the lift owes the frame nothing until it has a ground.
    #[test]
    fn a_chip_lift_with_no_latched_ground_draws_nothing() {
        let _g = plx_base::testlock::serial();
        let profile = crate::widgets::ProfileChipRead { src: 0, thumb: "", initial: c"G", name: c"Guest", name_w: 60.0 };
        let face = crate::widgets::chip_face(FACE, 1.0);
        let unlatched = crate::underlay::UnderlayField::new();
        for expand in [0.0, 1.0] {
            let all = declared_all(|| crate::widgets::lift_profile_chip(Painter::root(), profile, expand, Some(face), &unlatched));
            assert!(all.is_empty(), "unlatched ground, expand {expand}: no capsule, no avatar, no name; drew {:?}",
                all.iter().map(|p| p.kind()).collect::<Vec<_>>());
        }
    }

    /// **The rim's sharp source is the host snapshot**: `draw` hands the glass whatever
    /// `popover::host::page_tex` holds (0 when it holds no undimmed page), and nothing else — the
    /// glass takes no picture of its own.
    #[test]
    fn the_rims_sharp_source_is_the_host_snapshot() {
        let _g = plx_base::testlock::serial();
        let tex = 9_744;
        let field = latched(tex);
        let r = Rect::new(100.0, 100.0, 300.0, 200.0);
        let p = Painter::root();
        let sharp = crate::popover::host::page_tex().unwrap_or(0);
        let drawn = declared_glass(|| assert!(draw(p, r, 24.0, Some(&field))));
        assert!(drawn[0] == expected(p, r, 24.0, tex, sharp, FACE)[0]);
        unlatch(tex);
    }

    /// The tint arithmetic itself: scrim and rim alphas scale by the opacity.
    #[test]
    fn the_face_scales_every_alpha() {
        let f = face_at(0.1);
        assert!((f.scrim_top[3] - 0.78 * 0.1).abs() < 1e-6);
        assert!((f.scrim_bot[3] - 0.86 * 0.1).abs() < 1e-6);
        assert!((f.rim[3] - theme::GLASS_RIM[3] * 0.1).abs() < 1e-6);
        assert!((f.rim_lit[3] - theme::GLASS_RIM_LIGHT[3] * 0.1).abs() < 1e-6);
        assert_eq!(face_at(1.0).scrim_top, FACE.scrim_top);
        assert_eq!(face_at(0.0).rim_lit[3], 0.0);
    }

    /// The compact-menu dim is the board's 30% scrim.
    #[test]
    fn the_menu_scrim_is_thirty_percent() {
        assert_eq!(theme::underlay::DIM_COMPACT, 0.30);
    }
}
