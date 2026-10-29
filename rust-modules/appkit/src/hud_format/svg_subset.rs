//! **The SVG subset a HUD asset may use**, checked by a Rust tokenizer BEFORE the bytes reach nanosvg:
//! `src/svg.c` is a C parser and a package is attacker-controlled input.
//!
//! An asset is a MASK (`ui/src/icons.rs:9-10`: only alpha survives; the app tints it per state), so the
//! subset is the icon contract and nothing more: `<svg viewBox>` holding `<path d>`.
//!
//! **Policy** (the module's contract; [`check_asset`] enforces it and reports every violation it finds):
//! * Elements: `svg` and `path` only. Anything else is `HUD-E0701`; a gradient element
//!   (`linearGradient`, `radialGradient`, `stop`) carries the hint "HUD assets are alpha masks tinted by
//!   the app; a gradient would be discarded, so draw the mark in one colour".
//! * Attributes: `viewBox` (required), `xmlns` (exactly `http://www.w3.org/2000/svg`) and `fill` (the
//!   mask white `#ffffff` only) on `svg`; `d` (required), `fill` (mask white only) and `fill-rule` on
//!   `path`. Any other attribute, and any other `fill`, is `HUD-E0702` (a colour other than mask-white,
//!   `stroke*`, `opacity`, `style`, `class`, `transform`) with the hint "assets are masks tinted per
//!   state, so colour would be dropped". Mask-white is accepted because every icon under
//!   `assets/icons` carries `fill='#ffffff'`.
//! * Syntax the subset never admits is `HUD-E0703`: entities (`&`), `<!DOCTYPE`, CDATA, processing
//!   instructions, `href`/`xlink`, `url(`, `data:`, `://`, `<text>`'s and `<style>`'s content cannot be
//!   reached because those elements are already `HUD-E0701`.
//! * A malformed document (unbalanced tags, no `<svg viewBox>`, a `viewBox` that is not four finite
//!   numbers with positive width and height, a `d` that is not path data) is `HUD-E0704`.
//! * More than [`MAX_ASSET_BYTES`] bytes is `HUD-E0705`; more than [`MAX_PATHS_PER_ASSET`] `<path>`
//!   elements is `HUD-E0706`.
//!
//! Every finding's pointer is the asset string's own, the `pointer` argument (`/assets/<id>`).

use super::Finding;

/// The most bytes one asset may carry. Real icons are under 3 KiB; a published outline of a numeral set
/// is larger, and this bounds the tokenizer's work per asset.
pub const MAX_ASSET_BYTES: usize = 16 * 1024;
/// The most `<path>` elements one asset may carry (the real icons use one to four).
pub const MAX_PATHS_PER_ASSET: usize = 8;

/// What a checked asset is: its `viewBox` as `[min_x, min_y, width, height]` and its `<path>` count.
#[derive(Clone, Debug, PartialEq)]
pub struct SvgInfo {
    pub view_box: [f32; 4],
    pub paths: usize,
}

/// Check one asset against the subset. `pointer` is the asset string's JSON pointer (`/assets/gear`),
/// the pointer of every finding returned. `Ok` carries what the compiler needs to place the mask; `Err`
/// carries EVERY violation found (never empty).
// CONTRACT FREEZE: worker A fills this. This stub accepts every string and reports a 24x24 box with
// one path, so a parser that calls it compiles and its other rules can be exercised; it enforces
// nothing until A replaces it.
pub fn check_asset(_pointer: &str, _svg: &str) -> Result<SvgInfo, Vec<Finding>> {
    Ok(SvgInfo {
        view_box: [0.0, 0.0, 24.0, 24.0],
        paths: 1,
    })
}
