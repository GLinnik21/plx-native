/// **The residency ceiling for every render alive in one frame** (§8.3), derived — 64 MiB.
///
/// It was `48 << 20` and the doc said "a placeholder until phase 11 sets it from the 160 MB
/// `requiredMemory` measurement". Here is that arithmetic. Every measured figure is from
/// `docs/distribution.md` §6 item 10 (dev set M16p3, webOS 4.10.2, a `features=release` build,
/// 2026-08-22) and is a `VmHWM`/`VmRSS` reading in KiB, the units `/proc` uses:
///
/// ```text
///   declared budget   pkg/appinfo.json requiredMemory   160 MB
///   boot                                    VmHWM        35 MB
///   browsing Home/detail                    VmRSS   119,044 KiB
///     of which CPU              smaps_rollup           38,540 KiB
///     of which Mali textures    /proc/gpu 20,159 x 4 KiB 80,636 KiB
///   peak, with playback                     VmHWM   155,292 KiB
/// ```
///
/// 1. **The peak binds, not the browsing figure.** Browsing alone leaves 160 − 119 = 41 MB of
///    apparent room, but the browsing caches stay resident when playback starts — that is what
///    turns 119 into 155,292 KiB — so the headroom this ceiling may spend is measured from the
///    PEAK. Read `requiredMemory` as decimal MB (the conservative reading; as MiB it is 4.5 MiB
///    more generous, and this constant is chosen to hold under either): 156,250 − 155,292 = 958 KiB.
/// 2. **Texture memory is what this rule bounds.** `VmRSS` on this set INCLUDES the Mali pages —
///    proven arithmetically in that same measurement, 38,540 + 80,636 = 119,176 against a `VmRSS`
///    of 119,044 — so all texture memory may be at most 80,636 + 958 = 81,594 KiB ≈ 79.7 MiB.
/// 3. **The set is a SUBSET of that**, and the one large, exactly known exclusion is the blur
///    chain: `grab` 1920x1080 + `mid` 960x540 + `a`/`b` 480x270, RGBA8 = 11,404,800 B ≈ 10.9 MiB,
///    built once and never resized, so it is a fixed subtraction rather than a term.
///    79.7 − 10.9 = **68.8 MiB** for the screens' own renders, the `FrameCache`, `extra_bytes`,
///    and the two smaller exclusions (the 160-slot glyph cache, `ui::icons`).
/// 4. Round down to **64 MiB**, which leaves those two about 4.8 MiB and — the other side of the
///    same question — sits above the largest per-screen GPU residency ever measured here (the
///    detail hero, 54.5 MB whole-process; Home's grid 35.8 MB, the profile picker 11.7 MB). A
///    ceiling under that would assert on a healthy screen the moment `tex::resident_bytes()`
///    joins the sum, which is the failure mode a tighter number buys.
/// 5. **`ui::tex::TEX_RESIDENT_BYTES_MAX` (44 MiB) is the pool's OWN ceiling under this one** —
///    it is what actually bounds `tex::resident_bytes()`, the term this constant's derivation
///    leaves room for above; that module's doc has its own arithmetic against this constant.
///
/// **What is not settled.** Every figure above is whole-PROCESS: nothing has yet measured what
/// this set's own sum reads per screen, because until phase 11 it could not be non-zero.
/// **TV session 7 leg 8** (`VmRSS` + `/proc/gpu/<pid>` per screen, panel off) is the measurement
/// that confirms or moves this constant — if the set's own sum on the detail hero comes back
/// within 10 % of this number, the number is wrong rather than the app, and it moves DOWN to fit
/// what is actually held; if a screen's whole-process GPU exceeds ~80 MiB, the app is over its
/// declaration and no ceiling here is the fix.
pub const RENDER_BYTES_MAX: usize = 64 << 20;
/// One full-viewport `FrameCache` (1920×1080×4), the single one a Cached host is served from.
pub const FRAME_CACHE_BYTES: usize = 1920 * 1080 * 4;

/// **What ONE screen holds of this frame's render residency** — the answer to `Screen::render_report`
/// (§8.3), and the only thing the [`RenderSet`] learns from a screen.
///
/// `textures` counts the GL textures the screen OWNS and keeps across frames — the ones its own
/// code created and its own code deletes. It is not a count of draws, and a texture the screen
/// merely SAMPLES from a shared pool (`ui::tex`'s poster cache, `ui::icons`, the glyph cache, the
/// one `popover::host` `FrameCache`, the blur chain) belongs to that pool and is counted once
/// there, never per screen that looks at it — otherwise every screen showing a poster would report
/// the whole poster cache and the sum would be meaningless. That is exactly the distinction rule
/// (b) exists to make: "a Cached host holds NO render of its own (it is a quad from the one shared
/// `FrameCache`, never a second full-viewport texture)".
///
/// `bytes` is those textures' backing store, `w * h * 4` for the RGBA8 this renderer uploads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderReport {
    /// Backing render textures this screen owns.
    pub textures: u32,
    /// Their bytes.
    pub bytes: usize,
}

impl RenderReport {
    /// The default every screen inherits: it draws immediate-mode and owns no texture of its own.
    pub const NONE: Self = Self {
        textures: 0,
        bytes: 0,
    };

    /// One texture of `w * h` RGBA8 pixels.
    pub const fn one(w: u32, h: u32) -> Self {
        Self {
            textures: 1,
            bytes: (w as usize) * (h as usize) * 4,
        }
    }
}

/// Every `ScreenRender` alive this frame (§8.3): the frame plan's list, checked as a whole rather
/// than as the page pair alone.
#[derive(Clone, Debug, Default)]
pub struct RenderSet {
    /// Page renders drawn: the top page, plus the level beneath it under a push.
    pub pages: u32,
    /// `(surface, renders)` per Active surface — the surface's OWN backing render textures
    /// ([`RenderReport::textures`]), which is zero for every surface served from the shared
    /// `FrameCache`.
    pub surfaces: Vec<(crate::ui::machine::EntryId, u32)>,
    /// The sum of every drawn screen's own backing-texture bytes ([`RenderReport::bytes`]).
    pub bytes: usize,
    /// The one shared `FrameCache`, when a Cached host is being served from it.
    pub frame_cache_bytes: usize,
    /// **The shared render pools no screen owns**: `ui::tex::resident_bytes()` (the poster/logo
    /// cache's GL residency, conserved across eviction and replacement since phase 11), read by
    /// `Dispatcher::draw_with` when it opens the set. The glyph cache (160 slots) and `ui::icons`
    /// are the two small exclusions `RENDER_BYTES_MAX`'s derivation leaves room for; the blur
    /// chain (`gfx.rs`, ~11.4 MB and never resized) is accounted for in that derivation instead,
    /// because it is allocated once for the life of the process rather than per frame.
    pub extra_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderBreach {
    /// More than two page renders.
    Pages(u32),
    /// A surface holding more than one render.
    Surface(crate::ui::machine::EntryId, u32),
    /// The sum of every render plus the `FrameCache` is over `RENDER_BYTES_MAX`.
    Bytes(usize),
}

impl std::fmt::Display for RenderBreach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderBreach::Pages(n) => write!(f, "{n} page renders (max 2)"),
            RenderBreach::Surface(e, n) => write!(f, "surface {} holds {n} renders (max 1)", e.0),
            RenderBreach::Bytes(b) => write!(f, "{b} render bytes (max {RENDER_BYTES_MAX})"),
        }
    }
}

impl RenderSet {
    /// (a) pages ≤ 2, (b) one render per surface, (c) bytes + the `FrameCache` + the shared pools
    /// under the ceiling.
    pub fn check(&self) -> Result<(), RenderBreach> {
        if self.pages > 2 {
            return Err(RenderBreach::Pages(self.pages));
        }
        if let Some((e, n)) = self.surfaces.iter().find(|(_, n)| *n > 1) {
            return Err(RenderBreach::Surface(*e, *n));
        }
        let total = self.total_bytes();
        if total > RENDER_BYTES_MAX {
            return Err(RenderBreach::Bytes(total));
        }
        Ok(())
    }

    /// Every term of rule (c) in one place: the screens' own renders, the one `FrameCache`, and
    /// the shared pools [`RenderSet::extra_bytes`] carries.
    pub fn total_bytes(&self) -> usize {
        self.bytes + self.frame_cache_bytes + self.extra_bytes
    }
}

/// **What a breach DOES** (§8.3: "asserts (debug) and logs (release, once per breach)"), in one
/// place so both halves are reachable from a host test.
///
/// A debug build dies: a breach is a bug in the frame plan and the host suite is where it must be
/// paid for. A release build logs ONCE — a television that has started leaking renders would
/// otherwise write this line 60 times a second into the one debugging surface anybody can ask a
/// user for. `logged` is the caller's once-flag; the returned line is `None` when it has already
/// been spent.
///
/// In a release build this function IS [`breach_line`] — `debug_assert!` compiles out — which is
/// what makes `a_render_breach_logs_once_on_the_release_path` a test of the shipped path rather
/// than of a second copy of it.
pub fn on_breach(breach: &RenderBreach, logged: &mut bool) -> Option<String> {
    debug_assert!(false, "render set breach: {breach}");
    breach_line(breach, logged)
}

/// The once-per-process half of [`on_breach`], without the assertion.
pub fn breach_line(breach: &RenderBreach, logged: &mut bool) -> Option<String> {
    if *logged {
        return None;
    }
    *logged = true;
    Some(format!("dispatch: render set breach: {breach}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::machine::EntryId;

    /// The RELEASE half of the breach policy: one line, ever, however many frames breach — and it
    /// names which rule and by how much. In a release build `on_breach` is this function, the
    /// `debug_assert!` above it having compiled out.
    #[test]
    fn a_render_breach_logs_once_on_the_release_path() {
        let mut logged = false;
        let first = breach_line(&RenderBreach::Surface(EntryId(7), 2), &mut logged)
            .expect("the first breach is logged");
        assert!(first.contains("surface 7 holds 2 renders (max 1)"), "{first}");
        assert_eq!(
            breach_line(&RenderBreach::Bytes(usize::MAX), &mut logged),
            None,
            "…and every later one, of any rule, is silent"
        );
    }

    /// The DEBUG half: the same breach is fatal on the host, which is what keeps a frame plan that
    /// has started leaking renders from reaching a television at all.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "render set breach")]
    fn a_render_breach_asserts_on_the_debug_path() {
        let mut logged = false;
        let _ = on_breach(&RenderBreach::Pages(3), &mut logged);
    }

    /// **The ceiling against the measurement it was derived from** (see [`RENDER_BYTES_MAX`]'s
    /// own doc for the argument). Two-sided on purpose: too high and the app breaches its declared
    /// `requiredMemory` before this rule says a word; too low and it asserts on a screen that has
    /// already been measured healthy. Change the constant and this test makes you redo the
    /// arithmetic rather than nudge a number.
    #[test]
    fn the_render_ceiling_fits_the_declared_memory_budget() {
        // docs/distribution.md §6 item 10, 2026-08-22, dev set, features=release.
        const REQUIRED_MEMORY_KIB: usize = 160_000_000 / 1024; // 160 MB read as DECIMAL: the
                                                               // conservative of the two readings
        const VMHWM_PLAYBACK_KIB: usize = 155_292;
        const GPU_BROWSING_KIB: usize = 80_636;
        const HERO_SCREEN_GPU_BYTES: usize = 54_500_000; // the largest per-screen figure measured
        // The blur chain: allocated once, never resized, never a term in this set.
        const BLUR_CHAIN_BYTES: usize =
            1920 * 1080 * 4 + 960 * 540 * 4 + 2 * (480 * 270 * 4);

        let headroom_kib = REQUIRED_MEMORY_KIB - VMHWM_PLAYBACK_KIB;
        let texture_allowance = (GPU_BROWSING_KIB + headroom_kib) * 1024;
        assert!(
            RENDER_BYTES_MAX + BLUR_CHAIN_BYTES <= texture_allowance,
            "the ceiling plus the blur chain must fit the texture memory the declaration allows: \
             {RENDER_BYTES_MAX} + {BLUR_CHAIN_BYTES} > {texture_allowance}"
        );
        assert!(
            RENDER_BYTES_MAX > HERO_SCREEN_GPU_BYTES,
            "…and must not sit under a screen that has been measured healthy"
        );
    }

    /// Rule (c) sums three terms, and the third one is the hook the loop fills.
    #[test]
    fn the_byte_ceiling_covers_the_screens_the_frame_cache_and_the_shared_pools() {
        let set = RenderSet {
            bytes: 1,
            frame_cache_bytes: FRAME_CACHE_BYTES,
            extra_bytes: 2,
            ..Default::default()
        };
        assert_eq!(set.total_bytes(), FRAME_CACHE_BYTES + 3);
        let over = RenderSet {
            extra_bytes: RENDER_BYTES_MAX + 1,
            ..Default::default()
        };
        assert_eq!(over.check(), Err(RenderBreach::Bytes(RENDER_BYTES_MAX + 1)));
    }
}
