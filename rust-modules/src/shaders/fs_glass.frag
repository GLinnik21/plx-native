// The GLASS panel backdrop: the blurred snapshot sampled through a rounded slab that REFRACTS at
// its edges, so a popover reads as a thick object over the page rather than as a flat frosted
// rectangle. Apple's Liquid Glass is the reference for the behaviour, not for any implementation.
//
// It is nearly free, and the reason is that the LENS IS THE SDF WE ALREADY COMPUTE. `fs_img.frag`
// and `fs_src.frag` evaluate `sdBox` for every panel fragment to round its corners and antialias
// its edge; the distance that falls out of that is exactly the "how deep into the bevel am I"
// term a refraction needs, and the gradient of the same field is the surface normal to bend along.
// So the whole effect is a perturbed texture coordinate: NO extra pass, NO extra render target,
// and in the interior not even an extra fetch, because the early-out below returns before any of
// it runs.
//
// Three parts, in the order they matter:
//
//   1. **The lens.** `t` ramps 0→1 across `u_bevel` px inward from the edge, squared so the bend
//      is concentrated at the rim the way a real chamfer concentrates it. The UV is pushed OUTWARD
//      along the normal, which pulls what lies just beyond the panel into its rim. The displacement
//      is large on purpose: a blurred field has only coarse structure left, so a small offset moves
//      nothing the eye can see, and the effect has to come from sliding whole light and dark
//      regions rather than from any detail inside them.
//   2. **A DIRECTIONAL edge light.** Not a uniform ring: a slab lit from above has a bright top
//      chamfer and a dark bottom one, and that asymmetry is most of what says "this is a solid
//      object at an angle" rather than "this is a rectangle with a glow". `dot(normal, u_light.xy)`
//      — the normal is already in hand for the lens — lights the facing side and shades the other
//      by `u_light.z`.
//   3. **Dither.** A blurred field is nearly a gradient, and a gradient in 8 bits BANDS — the more
//      so where the lens stretches it. `GL_DITHER` is off on this part and `ui/widgets.rs` already
//      abandoned one construction over the same staircase, so the noise is not optional polish. It
//      is ±half a quantum, below the threshold of being seen as grain and above the one that turns
//      a band into a dither.
//
// **A REJECTED fourth part, recorded because it is the obvious idea and it is wrong.** The first
// version mixed the rim toward a SHARPER level of the chain (the half-res one, already allocated
// and otherwise idle), reasoning that real glass compresses the background at its edge rather than
// dissolving it, and that a quarter-res blur has nothing left to compress. Built and looked at on
// the television: it reads as the panel being **thinner at the edge**, not as refraction. Clarity
// is how a material says how much of it there is — changing it across the panel says the glass
// tapers, which is not what a bevel does. It also left a visible seam wherever the sharp and
// blurred levels disagreed, i.e. on exactly the high-contrast edges the effect was meant to show
// off. The material's opacity is uniform now, and only the GEOMETRY varies. That also gave the
// renderer its second texture unit back.
//
// PRECISION: every coordinate chain is highp, for the reason fs_src.frag's note spells out — fp16
// on a card-sized quad is off by ~half a texel, and here that error IS the refraction offset, so a
// mediump lens would ripple along a straight edge instead of bending evenly.
precision mediump float;
varying highp vec2 v_cuv; // where this fragment sits in the snapshot
varying highp vec2 v_p;   // panel-local pixel coords, for the SDF
uniform sampler2D u_tex;   // the blurred snapshot
uniform vec4 u_tint;
uniform highp float u_iradius;
uniform highp vec2 u_ch;    // panel half-size in px
uniform highp vec2 u_uvpx;  // UV delta per screen pixel, v axis included (so it may be negative)
uniform highp float u_bevel; // how far in from the edge the lens reaches, px
uniform highp float u_lens;  // peak displacement at the rim, px
uniform vec4 u_edge;         // edge-light colour; alpha 0 disables
uniform vec3 u_light;        // xy = direction TO the light, panel-local; z = counter-side shading
uniform vec4 u_spec;         // xy = the specular AXIS; z = tightness; w = strength (0 disables)
uniform float u_noise;       // dither amplitude
uniform float u_rimw;        // rim width in px, for the band above
// THE CONTAINER'S OWN SCRIM AND EDGE, composited HERE rather than as a second draw on top.
// A translucent panel used to be two surfaces of one shape — the backdrop, then a rimmed rect over
// it — and two surfaces means two antialiased edges, each blending its own colour against the page.
// The glass's edge paints the BLURRED interior, which along the bottom of a bar is content from
// UNDER it: measured on the light material, a half-covered boundary pixel came out at 214 against a
// 220 page and a 236 face, darker than either side, and on a curve that reads as a dotted line
// following the arc. One surface has one coverage and cannot do that. (Double antialiasing was NOT
// the culprit and was measured first: it costs two codes, not fourteen.)
uniform vec4 u_scrim_top;    // the scrim's stops, top and bottom; alpha 0 disables the whole block
uniform vec4 u_scrim_bot;
uniform vec4 u_rimcol;       // the container's perimeter line, over the scrim
// THE LIT EDGE IS ITS OWN COLOUR, and that is not a refinement — it is the difference between the
// two polarities being one material and being two. The lamp is above; the grain facing it catches
// light whether the surface is dark or light, so the HIGHLIGHT is white in both. What the polarity
// changes is the PERIMETER: white on a dark material, ink on a light one, where it reads as contact
// rather than as light. Carried as one weight on one colour, the light material's top edge came out
// the darkest thing on the bar, which is a lamp underneath the floor.
uniform vec4 u_rimlit;       // colour + weight of the edge facing the light (alpha 0 = none)

highp float sdBox(highp vec2 p, highp vec2 b, highp float r){
  highp vec2 q = abs(p) - b + vec2(r);
  return length(max(q,0.0)) + min(max(q.x,q.y),0.0) - r;
}
// The gradient of the field above: the outward normal. Analytic rather than by finite difference,
// which would cost two more sdBox evaluations for the same answer. Two cases, exactly matching the
// two terms of sdBox — the rounded corner (either component of q positive) and the flat sides.
highp vec2 sdBoxNormal(highp vec2 p, highp vec2 b, highp float r){
  highp vec2 s = sign(p);
  highp vec2 q = abs(p) - b + vec2(r);
  highp vec2 m = max(q, 0.0);
  if (m.x > 0.0 || m.y > 0.0) return s * normalize(m + vec2(1e-5));
  return s * (q.x > q.y ? vec2(1.0, 0.0) : vec2(0.0, 1.0));
}
float hash(highp vec2 p){ return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }

void main(){
  highp float d = sdBox(v_p, u_ch, u_iradius);
  // INTERIOR EARLY-OUT, the same shape as fs_src.frag's and for the same reason: past the bevel
  // there is no lens, no edge light and full coverage, so a panel's large flat middle pays one
  // fetch and nothing else. The branch is coherent across a tile, which is what makes it pay here.
  if (d < -u_bevel) {
    vec3 flat_rgb = texture2D(u_tex, v_cuv).rgb * u_tint.rgb;
    // The scrim reaches here too — this is most of the surface. The rim does not: it is a band at
    // the edge and `rimShape` below is zero this far in, which is what makes skipping it exact.
    vec4 fsc = mix(u_scrim_top, u_scrim_bot, clamp(v_p.y / u_ch.y * 0.5 + 0.5, 0.0, 1.0));
    flat_rgb = mix(flat_rgb, fsc.rgb, fsc.a);
    flat_rgb += (hash(gl_FragCoord.xy) - 0.5) * u_noise;
    gl_FragColor = vec4(flat_rgb, u_tint.a);
    return;
  }
  highp float t = clamp(1.0 + d / u_bevel, 0.0, 1.0); // 0 at the bevel's inner edge, 1 at the rim
  highp float lens = t * t;
  highp vec2 nrm = sdBoxNormal(v_p, u_ch, u_iradius);
  vec3 rgb = texture2D(u_tex, v_cuv + nrm * (lens * u_lens) * u_uvpx).rgb * u_tint.rgb;
  // The chamfer's light. `band` peaks just INSIDE the rim rather than on it — on it the coverage
  // mask below eats most of it and the line reads as aliasing instead of as a highlight. `ndl` is
  // what makes the panel look tilted rather than outlined: the facing chamfer takes the light, the
  // opposite one is shaded, and the two together are the only cue in here that has a direction.
  float band = smoothstep(0.55, 1.0, t) * (1.0 - smoothstep(0.94, 1.0, t));
  float ndl = dot(nrm, u_light.xy);
  rgb += u_edge.rgb * (band * max(ndl, 0.0) * u_edge.a);
  rgb *= 1.0 + band * min(ndl, 0.0) * u_light.z;
  // THE SPECULAR, and the reason it is `abs` rather than `max`: a glass rim reflects on the two
  // edges that lie along the light's axis, so it catches at OPPOSITE corners — top-left and
  // bottom-right for a lamp on that diagonal — while the other two stay dark. One lobe would be a
  // gradient down the panel; two are what read as a reflection. `u_spec.z` tightens it onto the
  // corner ARCS: on a straight edge the normal is constant, so without a high power the term
  // lights whole sides evenly and the corners stop being where anything happens.
  // Its band sits nearer the rim than the chamfer's, because a reflection is a highlight ON the
  // edge while the shading is the thickness BEHIND it.
  // A HAIRLINE, measured in pixels off the edge rather than as a fraction of the bevel: ~1.5 px
  // just inside the boundary. The soft wide band this replaced read as a glow around the panel;
  // what a glass rim actually shows is a fine bright line that follows the whole perimeter, and
  // its thinness is most of why it reads as an edge rather than as lighting.
  float hair = smoothstep(-2.2, -0.9, d) * (1.0 - smoothstep(-0.6, 0.4, d));
  // `u_spec.w` is the line's FLOOR strength — it runs all the way round — and the two-lobe term
  // adds to it at the corners the light axis points at. A line that vanishes between the lobes
  // stops being a perimeter and becomes two marks.
  float lobe = pow(abs(dot(nrm, u_spec.xy)), u_spec.z);
  rgb += u_edge.rgb * (hair * u_spec.w * (0.45 + 0.55 * lobe));
  // The container's own SCRIM, then ITS RIM on top of that, both before coverage — the order they
  // are read in, and the reason this surface is now the only one of its shape.
  vec4 sc = mix(u_scrim_top, u_scrim_bot, clamp(v_p.y / u_ch.y * 0.5 + 0.5, 0.0, 1.0));
  rgb = mix(rgb, sc.rgb, sc.a);
  // A LERP, not an addition, and that is the whole of why a DARK edge can exist at all. The rim was
  // `rgb += rimcol * weight`, which can only ever ADD light: a near-black rim added near-nothing, so
  // the light material simply had no edge once the black ring that had been standing in for one was
  // fixed. Mixing TOWARD the colour lets one expression draw a white line on a dark material and an
  // ink line on a light one, at the same stated weight.
  float rimShape = smoothstep(-u_rimw - 0.75, -u_rimw + 0.75, d) * (1.0 - smoothstep(-0.5, 0.5, d));
  float rimw = clamp(rimShape * u_rimcol.a, 0.0, 1.0);
  rgb = mix(rgb, u_rimcol.rgb, rimw);
  // …then the highlight, OVER the perimeter, on the side facing the light. +y is DOWN, so the top
  // edge's normal is (0,-1): full weight there, nothing at either cap's widest point, nothing along
  // the bottom. It dies out ALONG the arc rather than being scissored on it — a hard cut lands at
  // the widest point of a cap and reads as the outline snapping off in mid-air.
  float litw = clamp(rimShape * u_rimlit.a * max(-nrm.y, 0.0), 0.0, 1.0);
  rgb = mix(rgb, u_rimlit.rgb, litw);
  rimw = max(rimw, litw);
  rgb += (hash(gl_FragCoord.xy) - 0.5) * u_noise;
  // Coverage in the alpha only — see the note in fs_src.frag. The interior early-out above already
  // emits `flat_rgb` unpremultiplied, so this is also what makes the two exits of this shader agree:
  // they disagreed by a factor of `cov` for every fragment in the bevel band.
  // THE RIM MAY EXCEED THE SURFACE'S OWN COVERAGE, and it has to. `rimShape` peaks half a pixel
  // outside the boundary, exactly where `cov` is already falling, so a rim bounded by coverage is a
  // rim that fades out with the edge it is supposed to be drawing — measured, the dark material's
  // white top line dropped from 249 to 210 against a 220 ground, i.e. from a hairline to nothing.
  // Raising the alpha is what the old second surface did with `a = max(a, rim)`; this is the same
  // rule, now inside the one surface.
  float cov = 1.0 - smoothstep(-1.0, 1.0, d);
  gl_FragColor = vec4(rgb, max(u_tint.a * cov, clamp(rimw, 0.0, 1.0)));
}
