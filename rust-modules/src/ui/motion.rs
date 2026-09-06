//! Motion arithmetic the logical state can DEPEND on (spec §4.2): the spring integrators, with
//! their own pure-Rust `exp` and `sin_cos` (range-reduced polynomials, pinned bit for bit), so
//! that everything a machine hashes is IEEE `+ − × ÷ √` plus two functions whose every bit this
//! crate decides. Everything else on the platform libm (`f32::exp`, `sin_cos`) is fine for
//! pixels and refused for state by the `check-deps` libm gate.
//!
//! Two assumptions are STATED and CHECKED rather than assumed: rustc does not contract into FMA
//! without a flag (the grep gate on `fp-contract`/`fast-math`/`+fma`), and the armv7 soft-float
//! routines are correctly rounded for the five operations — which is what
//! [`differential_table`] exists to measure: the same 4,096 operands through the same code on
//! the host and on the television (`make softfloat-probe`, the `plxnative-softfloat` trigger),
//! compared as one hash. The host half is pinned here; the ARM half is TV session 3 (phase 5b).
//! If they diverge, the cross-target fixture is target-scoped and the divergence named — the
//! same-build promise (§5.5) is not widened.
//!
//! Domain: the springs feed `exp` with `-ω·dt ∈ [-2, 0]` and `sin_cos` with `ω_d·dt ∈ [0, 2]`;
//! both functions are correct well beyond that (`exp` over the whole finite range, `sin_cos` to
//! |x| ≈ 1e4 with three-part Cody-Waite reduction) and say what they do past it.
#![allow(dead_code)] // phase 2: the reporting integrators gain callers as screens migrate

use super::machine::{PresentHandle, Tick};
use super::present::PresentEvent;

// --- exp ----------------------------------------------------------------------------------------

const LOG2E: f32 = 1.442_695_04;
/// ln 2 split so that `k * LN2_HI` is exact in f32 for |k| < 2^10 (HI has 16 significant bits).
const LN2_HI: f32 = 0.693_145_751_953_125;
const LN2_LO: f32 = 1.428_606_765_330_187e-6;

/// e^x. Overflow saturates to +∞, underflow past 2^-126 flushes to 0 (no denormal tail — a spring
/// envelope of 1e-38 is zero for every purpose here). NaN in, NaN out.
pub fn exp(x: f32) -> f32 {
    if x.is_nan() {
        return x;
    }
    if x > 88.72 {
        return f32::INFINITY;
    }
    if x < -87.33 {
        return 0.0;
    }
    // x = k·ln2 + r, |r| ≤ ln2/2
    let kf = (x * LOG2E).round();
    let k = kf as i32;
    let r = (x - kf * LN2_HI) - kf * LN2_LO;
    // e^r by its Taylor polynomial to r^6: |r| ≤ 0.3466 keeps the truncation under 1.3e-7 relative
    let p = 1.0
        + r * (1.0
            + r * (0.5
                + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0))))));
    // × 2^k by building the exponent bits (k ∈ [-126, 127] after the range guards above)
    let scale = f32::from_bits(((k + 127) as u32) << 23);
    p * scale
}

// --- sin_cos ------------------------------------------------------------------------------------

const TWO_OVER_PI: f32 = 0.636_619_772;
/// π/2 in three parts, each exact in f32 with trailing zeros so `k * PIO2_x` is exact.
const PIO2_HI: f32 = 1.570_312_5;
const PIO2_MID: f32 = 4.837_512_969_970_703e-4;
const PIO2_LO: f32 = 7.549_789_948_768_648e-8;

/// (sin x, cos x). Reduced to a quadrant by three-part Cody-Waite, then odd/even polynomials on
/// |r| ≤ π/4 (absolute error < 2e-9). Past |x| = 1e4 the reduction loses bits and the answer is
/// (0, 1), which no integrator can reach: ω_d·dt is bounded by the clamp on `dt`.
pub fn sin_cos(x: f32) -> (f32, f32) {
    if !x.is_finite() || x.abs() > 1.0e4 {
        return (0.0, 1.0);
    }
    let kf = (x * TWO_OVER_PI).round();
    let r = ((x - kf * PIO2_HI) - kf * PIO2_MID) - kf * PIO2_LO;
    let r2 = r * r;
    let s = r
        * (1.0
            - r2 * (1.0 / 6.0 - r2 * (1.0 / 120.0 - r2 * (1.0 / 5040.0 - r2 * (1.0 / 362_880.0)))));
    let c = 1.0 - r2 * (0.5 - r2 * (1.0 / 24.0 - r2 * (1.0 / 720.0 - r2 * (1.0 / 40_320.0))));
    match (kf as i32) & 3 {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

// --- the integrators, reporting through the present handle ---------------------------------------

/// The rest test (`ui::idle`'s, verbatim): magnitude-relative, capped under a quarter pixel, the
/// velocity judged as the travel this frame.
const REST_REL: f32 = 1e-3;
const REST_CAP: f32 = 0.25;

fn moving(pos: f32, target: f32, vel: f32, dt: f32) -> bool {
    let t = (REST_REL * (1.0 + pos.abs().max(target.abs()))).min(REST_CAP);
    (pos - target).abs() > t || (vel * dt).abs() > t
}

/// Critically-damped spring step — the exact analytic solution of `x'' + 2ω·x' + ω²·x = 0`
/// (`gfx::spring`'s form, on this module's `exp`), reporting `Motion` while it moves.
pub fn spring(
    pos: &mut f32,
    vel: &mut f32,
    target: f32,
    k: f32,
    t: Tick,
    present: &mut PresentHandle<'_>,
) {
    let dt = t.dt();
    let w = k.sqrt();
    let e = exp(-w * dt);
    let x = *pos - target;
    let b = *vel + w * x;
    *pos = target + (x + b * dt) * e;
    *vel = (*vel - w * b * dt) * e;
    if moving(*pos, target, *vel, dt) {
        present.note(PresentEvent::Motion);
    }
}

/// Underdamped spring step (`gfx::spring_zeta`'s form, on this module's `exp` and `sin_cos`).
pub fn spring_zeta(
    pos: &mut f32,
    vel: &mut f32,
    target: f32,
    k: f32,
    zeta: f32,
    t: Tick,
    present: &mut PresentHandle<'_>,
) {
    let dt = t.dt();
    let w = k.sqrt();
    let z = zeta.clamp(0.0, 0.999);
    let wd = w * (1.0 - z * z).sqrt();
    let x0 = *pos - target;
    let v0 = *vel;
    let e = exp(-z * w * dt);
    let (s, c) = sin_cos(wd * dt);
    let a = x0;
    let b = (v0 + z * w * x0) / wd;
    *pos = target + e * (a * c + b * s);
    *vel = e * ((b * wd - z * w * a) * c - (a * wd + z * w * b) * s);
    if moving(*pos, target, *vel, dt) {
        present.note(PresentEvent::Motion);
    }
}

/// A clock-driven ramp (`Xfade`'s shape) that REPORTS from inside `advance`, which is what the two
/// animators that shipped frozen (`Xfade`, `Spinner`) lacked.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ramp {
    pub at_ms: u32,
    pub len_ms: u32,
    pub running: bool,
}

impl Ramp {
    pub fn start(&mut self, t: Tick, len_ms: u32) {
        self.at_ms = t.ms;
        self.len_ms = len_ms.max(1);
        self.running = true;
    }

    /// 0..1 progress; reports `Motion` while running and stops itself at the end.
    pub fn advance(&mut self, t: Tick, present: &mut PresentHandle<'_>) -> f32 {
        if !self.running {
            return 1.0;
        }
        let el = t.ms.wrapping_sub(self.at_ms);
        if el >= self.len_ms {
            self.running = false;
            return 1.0;
        }
        present.note(PresentEvent::Motion);
        el as f32 / self.len_ms as f32
    }
}

// --- the differential table -------------------------------------------------------------------

/// How many operand pairs the table holds.
pub const DIFFERENTIAL_N: usize = 4096;

fn lcg(s: &mut u32) -> u32 {
    *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *s
}

/// A pseudo-random finite f32 in roughly ±2^12 with a full mantissa, from the LCG.
fn operand(s: &mut u32) -> f32 {
    let bits = lcg(s);
    let mant = bits & 0x007f_ffff;
    let exp = 115 + ((bits >> 23) & 0x1f); // 2^-12 .. 2^19
    let sign = bits & 0x8000_0000;
    f32::from_bits(sign | (exp << 23) | mant)
}

/// The 4,096-operand table: for each pair `(a, b)`, the bits of `a+b`, `a-b`, `a*b`, `a/b`,
/// `sqrt(|a|)`, `exp(a/2^16)`, `sin(b/2^12)`, `cos(b/2^12)` — eight words per pair.
pub fn differential_table(out: &mut Vec<u32>) {
    out.clear();
    out.reserve(DIFFERENTIAL_N * 8);
    let mut s = 0x5eed_1234u32;
    for _ in 0..DIFFERENTIAL_N {
        let a = operand(&mut s);
        let b = operand(&mut s);
        let (sn, cs) = sin_cos(b / 4096.0);
        for v in [
            a + b,
            a - b,
            a * b,
            a / b,
            a.abs().sqrt(),
            exp(a / 65_536.0),
            sn,
            cs,
        ] {
            out.push(v.to_bits());
        }
    }
}

/// FNV-1a over the table words: the one number the host and the television compare.
pub fn differential_hash() -> u64 {
    let mut t = Vec::new();
    differential_table(&mut t);
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for w in t {
        for b in w.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// The host half of the differential claim, as a pinned constant. Re-pinning it is a deliberate
/// act that must name why the arithmetic changed.
pub const DIFFERENTIAL_HASH_HOST: u64 = 0x65a8_e905_a259_246d;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::present::Present;

    #[test]
    fn exp_and_sin_cos_are_close_to_libm_over_the_spring_domain() {
        let mut worst_e = 0.0f32;
        let mut worst_s = 0.0f32;
        for i in 0..=4000 {
            let x = -2.0 + i as f32 * 0.001; // exp domain [-2, 2]
            let rel = ((exp(x) - x.exp()) / x.exp()).abs();
            worst_e = worst_e.max(rel);
            let y = i as f32 * 0.005; // sin_cos domain [0, 20]
            let (s, c) = sin_cos(y);
            let (ls, lc) = y.sin_cos();
            worst_s = worst_s.max((s - ls).abs()).max((c - lc).abs());
        }
        assert!(worst_e < 4e-7, "exp relative error {worst_e}");
        assert!(worst_s < 4e-7, "sin_cos absolute error {worst_s}");
        assert_eq!(exp(0.0), 1.0);
        assert_eq!(sin_cos(0.0), (0.0, 1.0));
        assert!(exp(100.0).is_infinite() && exp(-100.0) == 0.0);
        assert!(exp(f32::NAN).is_nan());
    }

    /// The bit-for-bit pin: these words are what THIS crate's `exp`/`sin_cos` produce for these
    /// inputs on the build that wrote them; a change to the polynomials or the reduction changes
    /// them, and the recorded fixtures with them (§5.5).
    #[test]
    fn motion_exp_and_sin_cos_match_the_pinned_table_bit_for_bit() {
        let inputs: [f32; 8] = [-2.0, -0.7, -0.05, 0.0, 0.3, 1.0, 1.6, 12.5];
        let got: Vec<u32> = inputs
            .iter()
            .flat_map(|&x| {
                let (s, c) = sin_cos(x);
                [exp(x).to_bits(), s.to_bits(), c.to_bits()]
            })
            .collect();
        assert_eq!(got.as_slice(), PINNED.as_slice(), "repin only with a named reason");
    }
    const PINNED: [u32; 24] = [
        0x3e0a9555, 0xbf68c7b7, 0xbed51132,
        0x3efe406e, 0xbf24eb73, 0x3f43ccb3,
        0x3f7383c6, 0xbd4cb6f5, 0x3f7fae19,
        0x3f800000, 0x00000000, 0x3f800000,
        0x3facc82c, 0x3e974e6d, 0x3f7490ef,
        0x402df854, 0x3f576aa4, 0x3f0a5140,
        0x409e7f3e, 0x3f7fe40e, 0xbcef33e2,
        0x48830629, 0xbd87d3c6, 0x3f7f6fb5,
    ];

    #[test]
    fn the_soft_float_differential_table_matches() {
        let h = differential_hash();
        assert_eq!(h, DIFFERENTIAL_HASH_HOST, "the host half moved: name why in the commit");
    }

    #[test]
    fn a_spring_reports_motion_until_it_rests() {
        let mut present = Present::new();
        let _ = present.take(0);
        let (mut pos, mut vel) = (0.0f32, 0.0f32);
        let t = Tick {
            ms: 0,
            dt_us: 16_667,
        };
        let mut frames = 0;
        loop {
            let mut ph = PresentHandle(&mut present);
            spring(&mut pos, &mut vel, 100.0, 300.0, t, &mut ph);
            frames += 1;
            if !present.take(frames * 16) {
                break;
            }
            assert!(frames < 600, "never rested");
        }
        assert!((pos - 100.0).abs() < 0.25 && frames > 5, "pos={pos} frames={frames}");
    }

    #[test]
    fn a_ramp_reports_motion_from_inside_advance() {
        let mut present = Present::new();
        let _ = present.take(0);
        let mut r = Ramp::default();
        r.start(Tick { ms: 0, dt_us: 0 }, 200);
        let mut ph = PresentHandle(&mut present);
        let p = r.advance(Tick { ms: 100, dt_us: 0 }, &mut ph);
        assert!((p - 0.5).abs() < 1e-6);
        assert!(present.take(100), "a running ramp presents");
        let mut ph = PresentHandle(&mut present);
        assert_eq!(r.advance(Tick { ms: 250, dt_us: 0 }, &mut ph), 1.0);
        assert!(!r.running);
        assert!(!present.take(250), "a finished ramp does not");
    }
}
