// The tile texture shader is the whole CARD COMPOSITE - texture + the 1px focus edge-sheen
// (u_rimw/u_rimcol) + a soft drop-shadow (u_card/u_shinv/u_shcol) - all in ONE pass.
// Perf (Mali-T820, per the perf review): (1) an INTERIOR EARLY-OUT - ~85% of a card's fragments
// are strictly inside the rounded rect (d < -2) where rim/AA/shadow are all zero, so they skip the
// 4 smoothsteps; on this per-thread tiler the branch genuinely saves the ALU. (2) UV remap +
// card-local p are interpolated varyings, not per-fragment math (see vs_img.vert). (3) the
// uniform-only terms (u_card's corner coordinates and interior cutoff, u_shinv = 0.5/blur) are folded on the CPU
// (Midgard has no uniform pre-shader). (4) the 1px rim is a single-op triangle - and its width
// must stay <=1px: the triangle hits exactly 0 at d=-2, which is what makes the d<-2 early-out
// seamless (a wider rim would be hard-cut there). The shadow sh = smoothstep(clamp(0.5 -
// d/(2*blur))) is algebraically identical to 1 - smoothstep(-blur, blur, d). rgb = tex*m
// premultiplies coverage (so a rounded texture's ~1px AA edge is very slightly darker under
// straight-alpha blend - accepted). Full-screen art (radius 0) takes the flat fast-path.
// v_cuv/v_p and the SDF chain are highp - see the PRECISION note in fs_src.frag.
//
// PLX_FOCUS (linked as its own program, gfx.rs's FOCUS_IMAGE/VS_FOCUS/FS_FOCUS): the lit-glass
// edge and risen shadow for a FOCUSED ArtTile used to be inline here unconditionally, behind
// `u_focus.x > 0.0` uniform branches. A TV A/B measured real backpressure from that: EVERY IPROG
// draw (every resting card, glyph, blur reduction, `field_kick`, `FrameCache` quad) paid for the
// bigger program even with nothing focused - Midgard flattens the extra uniform branches into
// selects, and the `v_gloss` varying load costs something on every fragment regardless of
// `u_focus.x`. The plain (non-`PLX_FOCUS`) path below is therefore BYTE-FOR-BYTE the pre-focus
// program (main's `fs_img.frag` at 78a823eea, comments aside): `IPROG`, `FS_STILL` and every
// resting-card draw compile and run exactly that, unaffected by any of this file's FOCUS code.
// `gfx::draw_tex_impl` only reaches for the `PLX_FOCUS` program when a draw is actually focused or
// risen (`focus > 0.0 || dy > 0.0` - at most one card on screen), falling back to the plain
// program if it fails to link.
//
// FOCUS (ArtTile, theme.rs's CARD_GLOW_*/CARD_GLARE_*/CARD_GLOSS_* constants document these same
// numbers - GLSL has no way to read a Rust const, so the numbers below are literals and
// `image_focus_geometry_matches_the_shader_literals` in gfx.rs pins the two together): `u_focus.x`
// is the caller's pop factor f (0 at rest), the SAME value for every fragment of one draw call, so
// every `u_focus.x > 0.0` branch below is a uniform branch. Three additive layers, white only:
// (1) RIM, an inward glow that follows the TRUE rounded outline (a plain box distance is only
// correct on the straight segments - a circle's corner distance is `length(q)`, not `max(q.x,q.y)`,
// and CSS inset box-shadow itself follows `border-radius`): the base band (.08, 12px) and the
// brighter-top (.12, 6px)/fainter-bottom (.06, 4px) bands are all `clamp(1 + d/px, 0, 1)` against
// the SAME `d` the SDF/rim/shadow already compute, so there is no second distance field - only extra
// `clamp`s once `d` is known. The top/bottom bands are further weighted by `vp.y` so they read as
// "this edge", not "near any edge": full weight hugging the top/bottom straight run, fading toward
// the sides and following the corner's own falloff through `d`, which is what makes it look the same
// on a circle as on a rounded rect. (2) GLARE, the top tab bar's own glass crown: the 1px perimeter
// sheen is lifted from u_rimcol.a towards CARD_GLARE_A (0.45, brighter than the design system's
// GLASS_RIM_LIGHT .28 - a hairline over bright artwork needs more contrast than one over the dark
// glass track GLASS_RIM_LIGHT was tuned for) for the top 12px, easing back to the plain sheen by 16%
// of the tile height (CARD_GLARE_EASE, which keeps the glare on the crown only rather than running
// down the sides) - folded into the existing `rim` term, no extra SDF. (3) GLOSS, a
// linear-gradient(160deg, white .14 0%, transparent 34%) over the face from the top-left corner -
// AFFINE in position, so `vs_img.vert` computes it once per vertex as `v_gloss` and this file only
// ever reads the interpolated result, never re-derives it (`u_focus.y`, 1/gradient-length, is the
// one CPU-folded term the vertex shader also needs). Because gloss is free, both fast-exit branches
// below (the box-interior one and the post-`d` one) apply it directly and return - only the ≤12px
// true border band still takes the full SDF/RIM/GLARE/shadow path, and its own combined
// `glow`/`gloss` mix is ONE `clamp`ped mix, not two sequential ones - that IS the algebra, not a
// style choice. `wide`/`wide2` are each a single box-distance compare: the top 6px/bottom 4px glare
// bands used to widen them too, but both are provably already covered by `edge < 12.0` (the
// top/bottom distances are each >= the box `edge`, so a narrower band's own cutoff can never fire
// outside it), and the gloss cutoff needs nothing wider now that the cheap gloss mix runs at the
// exit itself.
//
// SHADOW: a resting/plain-image draw's shadow is the ordinary symmetric penumbra, `sh` evaluated
// against the same `d` as the rim/AA above (a card sitting close to its shelf reads fine as a soft,
// even glow). A FOCUSED card instead reads as RISEN, its shadow falling below it rather than sitting
// evenly around it. The shift is not a second SDF query point subtracted from `v_p`:
// `gfx::draw_tex_impl` inflates the FOCUSED quad ASYMMETRICALLY (more room below than above - see
// its own comment), which moves the QUAD's centre `dy` px below the CARD's true centre by
// construction, so raw `v_p` (quad-centred, as `vs_img.vert` has always defined it) already IS the
// shadow's own coordinate with no correction at all - `dShadow` reads it directly. The CARD's own
// shape (`q`/`straight`/`d`/glow/glare/gloss), which still needs the TRUE card-centred coordinate,
// applies the opposite correction instead: `vp = v_p + (0, u_focus.z)`. A fragment whose shadow is
// definitely past its own falloff (checked with a cheap box-distance proxy, no second `length()`)
// AND whose card contribution is definitely zero exits fully transparent before any of this runs -
// `discard` is expensive on Mali, so this writes `vec4(0)` and returns instead.
precision mediump float;
varying highp vec2 v_cuv;
varying highp vec2 v_p;
#ifdef PLX_FOCUS
varying highp float v_gloss; // GLOSS's linear projection, computed once per vertex - see vs_img.vert
#endif
uniform sampler2D u_tex;
#ifndef PLX_STILL_GROUND
uniform vec4 u_tint;
#endif
uniform float u_rimw;
uniform vec4 u_rimcol;
uniform highp vec4 u_card; // half-size minus radius, radius, conservative interior threshold
uniform float u_shinv;
uniform vec4 u_shcol;
#ifdef PLX_FOCUS
uniform highp vec3 u_focus; // (pop factor f, 1/gloss-gradient-length, shadow y-offset px)
#endif
#ifdef PLX_STILL_GROUND
varying mediump float v_still_ramp;
uniform vec4 u_still_col;
// Compose the existing straight-alpha card output, then its separate SDF-clipped scrim.
// Emit premultiplied RGB for this specialized draw's GL_ONE blend: no divide or alpha branch
// enlarges the whole program's register budget. Zero/tiny alpha needs no special case.
// The near-black ink is a supplied theme color; treating it as zero changes the picture.
vec4 stillOver(vec3 rgb, float alpha, float coverage, float ramp){
  float s = ramp * coverage;
  float keep = alpha * (1.0 - s);
  return vec4(rgb * keep + u_still_col.rgb * s, s + keep);
}
#endif
void main(){
  vec4 c = texture2D(u_tex, v_cuv);
#ifdef PLX_STILL_GROUND
  // The CPU admits only an exactly-white tint to this specialization.
  vec3 tex = c.rgb;
  float ta = c.a;
#else
  vec3 tex = c.rgb*u_tint.rgb;
  float ta = c.a*u_tint.a;
#endif
#ifdef PLX_STILL_GROUND
  float ramp = u_still_col.a * clamp(v_still_ramp, 0.0, 1.0);
  if (u_card.z < 0.5) { gl_FragColor = stillOver(tex, ta, 1.0, ramp); return; }
#else
  if (u_card.z < 0.5) { gl_FragColor = vec4(tex, ta); return; }
#endif
#ifdef PLX_FOCUS
  // `vp` is the CARD-centred coordinate: raw `v_p` shifted back up by the shadow's own downward
  // offset, since the quad's centre (what `v_p` is relative to) now sits `u_focus.z` px BELOW the
  // card's true centre once the asymmetric shadow inflation is in play (0 otherwise - see the
  // SHADOW paragraph above). Every CARD-shape term (q/straight/d/glow/glare/gloss) reads `vp`;
  // only the shadow's own `dShadow` below reads raw `v_p`.
  highp vec2 vp = vec2(v_p.x, v_p.y + u_focus.z);
  highp vec2 q = abs(vp) - u_card.xy;
#else
  highp vec2 q = abs(v_p) - u_card.xy;
#endif
  highp float straight = max(q.x, q.y);
#ifdef PLX_FOCUS
  // The FIRST gate runs before `d` exists, so it can only afford the box distance (exact on the
  // straight segments, which is all `straight < u_card.w` ever admits) - gloss is a plain
  // interpolated varying now, so it costs nothing extra to apply right here rather than needing to
  // fall through to the slow path for it.
  bool wide = u_focus.x > 0.0 && (u_card.z - straight) < 12.0;
  if (straight < u_card.w && !wide) {
    if (u_focus.x > 0.0) {
      tex = mix(tex, vec3(1.0), clamp(1.0 - v_gloss / 0.34, 0.0, 1.0) * 0.14 * u_focus.x);
    }
#ifdef PLX_STILL_GROUND
    gl_FragColor = stillOver(tex, ta, 1.0, ramp);
#else
    gl_FragColor = vec4(tex, ta);
#endif
    return;
  }
#else
  if (straight < u_card.w) {
#ifdef PLX_STILL_GROUND
    gl_FragColor = stillOver(tex, ta, 1.0, ramp);
#else
    gl_FragColor = vec4(tex, ta);
#endif
    return;
  }
#endif
  float d = straight - u_card.z;
  if (min(q.x, q.y) > 0.0) d = length(q) - u_card.z;
#ifdef PLX_FOCUS
  // The SECOND gate has the true rounded `d`: the glow's widest band is 12px, so `d > -12.0` covers
  // every RIM layer (the top/bottom bands are narrower, 6px/4px) in one compare.
  bool wide2 = u_focus.x > 0.0 && d > -12.0;
  if (d < -2.0 && !wide2) {
    if (u_focus.x > 0.0) {
      tex = mix(tex, vec3(1.0), clamp(1.0 - v_gloss / 0.34, 0.0, 1.0) * 0.14 * u_focus.x);
    }
#ifdef PLX_STILL_GROUND
    gl_FragColor = stillOver(tex, ta, 1.0, ramp);
#else
    gl_FragColor = vec4(tex, ta);
#endif
    return;
  }
#else
  if (d < -2.0) {
#ifdef PLX_STILL_GROUND
    gl_FragColor = stillOver(tex, ta, 1.0, ramp);
#else
    gl_FragColor = vec4(tex, ta);
#endif
    return;
  }
#endif
#ifdef PLX_FOCUS
  // The shadow's own distance: raw `v_p`, never `vp` - see the SHADOW paragraph above for why the
  // unshifted quad coordinate is already exactly what the shadow needs. `u_focus.z == 0` collapses
  // `qs`/`straightS` to `q`/`straight` exactly (both read the same `vp == v_p`), so this costs
  // nothing extra for every draw but a focused card's.
  highp vec2 qs = q;
  highp float straightS = straight;
  if (u_focus.z > 0.0) {
    qs = abs(v_p) - u_card.xy;
    straightS = max(qs.x, qs.y);
  }
  // A box-only (no `length()`) lower bound on the true shadow distance - safe for a "definitely
  // past the falloff" test since the true rounded distance is never smaller than the box one.
  highp float dShadowBox = straightS - u_card.z;
  // Past this point the card's own AA/rim is exactly zero once `d >= 1.0` (`m`'s own smoothstep
  // ceiling - GLSL's `smoothstep` clamps to exactly 1.0 past its top edge, and `rim` is already 0
  // once `d >= 0.0`). If the shadow's box proxy has ALSO already cleared its own falloff, the whole
  // fragment is provably transparent without ever computing the shadow's `length()` - the bigger,
  // further-falling risen shadow otherwise pays that across ~35% more padded fragments than the
  // plain symmetric halo it replaced. `discard` is expensive on Mali, so this writes `vec4(0)`
  // directly rather than discarding.
  if (d >= 1.0 && (0.5 - dShadowBox * u_shinv) <= 0.0) {
    gl_FragColor = vec4(0.0);
    return;
  }
  float glareTop = 0.0;
  if (u_focus.x > 0.0) {
    // RIM: three additive inset layers against the SAME rounded `d` the rim/shadow below already
    // computed - `clamp(1 + d/px, 0, 1)` is 1 at the edge (d=0), fading to 0 by `px` inward, so it
    // follows the corner curve exactly as an inset `box-shadow` does. The top/bottom bands are also
    // weighted by `vp.y/chh` so they read as one edge's light, not a band that wraps every side.
    highp float chh = u_card.y + u_card.z;
    highp float glow = clamp(1.0 + d / 12.0, 0.0, 1.0) * 0.08
         + clamp(1.0 + d / 6.0, 0.0, 1.0) * 0.12 * clamp(-vp.y / chh, 0.0, 1.0)
         + clamp(1.0 + d / 4.0, 0.0, 1.0) * 0.06 * clamp(vp.y / chh, 0.0, 1.0);
    // GLARE: the crown's extra alpha over the resting rim, full for the top 12px, easing back to 0
    // (plain CARD_SHEEN) by 16% of the tile height (`chh * 0.32` is `(2*chh) * 0.16`, the full height
    // times the ease fraction CARD_GLARE_EASE, folded into the one constant `chh` is multiplied by).
    // `glareZone` is that same product, guarded against a degenerate tiny tile.
    highp float glareZone = max(chh * 0.32, 1.0);
    highp float ease = max(glareZone - 12.0, 1.0);
    highp float topDist = vp.y + chh;
    glareTop = clamp((glareZone - topDist) / ease, 0.0, 1.0) * (0.45 - u_rimcol.a) * u_focus.x;
    // GLOSS: linear-gradient(160deg, white .14 0%, transparent 34%) from the top-left corner,
    // `v_gloss` interpolated from `vs_img.vert` rather than re-derived per fragment.
    highp float gloss = clamp(1.0 - v_gloss / 0.34, 0.0, 1.0) * 0.14 * u_focus.x;
    tex = mix(tex, vec3(1.0), clamp(glow * u_focus.x + gloss, 0.0, 1.0));
  }
#endif
  float m = 1.0 - smoothstep(-1.0, 1.0, d);
#ifdef PLX_FOCUS
  float rim = max(0.0, 1.0 - abs(d + u_rimw)) * (u_rimcol.a + glareTop);
#else
  float rim = max(0.0, 1.0 - abs(d + u_rimw)) * u_rimcol.a;
#endif
  tex = mix(tex, u_rimcol.rgb, rim);
#ifdef PLX_FOCUS
  // The exact shadow distance. `u_focus.z <= 0` reuses `d` (exact already, `length()` and all) -
  // the box proxy above is only ever a safe LOWER bound, not the true rounded distance, so it must
  // not leak into the final `sh` for the unshifted case or a resting card's corner shadow would
  // read as a diamond instead of round.
  highp float dShadow = d;
  if (u_focus.z > 0.0) {
    dShadow = dShadowBox;
    if (min(qs.x, qs.y) > 0.0) dShadow = length(qs) - u_card.z;
  }
  float sh = clamp(0.5 - dShadow*u_shinv, 0.0, 1.0);
#else
  float sh = clamp(0.5 - d*u_shinv, 0.0, 1.0);
#endif
  sh = sh*sh*(3.0 - 2.0*sh) * u_shcol.a * (1.0 - m);
#ifdef PLX_STILL_GROUND
  gl_FragColor = stillOver(tex*m, ta*m + sh, m, ramp);
#else
  gl_FragColor = vec4(tex*m, ta*m + sh);
#endif
}
