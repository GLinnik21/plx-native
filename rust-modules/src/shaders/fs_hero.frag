// THE HERO GROUND IN ONE PASS: the backdrop photograph with both scrim fields applied to it
// analytically, instead of the photograph plus four blended quads drawn over it.
//
// The measured reason (dev television, Mali-T820, Home's hero with `plxnative-overdraw` armed):
// the hero submits 5.39M authored pixels a frame against a 2.07M-pixel panel — 2.60x — and 90% of
// that is three stacked full-panel layers. The art is 2,073,600 px; the frame-wide atmospheric ramp
// is two quads totalling 1,368,576; the corner wedge is two more totalling 1,410,048. The ramp and
// the wedge are pure GRADIENT: no texture, ~3 ALU each. Their whole cost is that they are 2.78M
// more fragments through the blender, and every one of them lands on a pixel the art already wrote.
//
// Both fields are cheap CLOSED FORMS of the authored pixel position, and both are the same ink
// (theme::SCRIM_INK) at different weights. So this shader evaluates both where the art already is:
//
//   a1  the atmospheric ramp — two linear segments meeting at a knee (home::base_scrim_a)
//   a2  the corner wedge — hero_scrim_a(x) feathered in over [top, knee] (widgets::hero_scrim_quads)
//
// Two straight-alpha layers of ONE ink compose exactly as a1 + a2 - a1*a2, so B below is what the
// two of them together do to whatever is under them. The art itself may be translucent (it fades
// with the snap dive), and the result must still be one draw over the ground the frame already
// holds, so the emitted fragment is the ALGEBRAIC composite of all three:
//
//   want   dst' = mix(mix(dst, art, A), ink, B)
//               = dst*(1-A)*(1-B) + art*A*(1-B) + ink*B
//   one straight-alpha blend gives dst*(1-s) + src*s, so
//   s      = 1 - (1-A)*(1-B)
//   src    = (art*A*(1-B) + ink*B) / s
//
// which is exact in real arithmetic. What differs from the four-quad path is 8-bit ROUNDING: that
// path quantises the framebuffer three times where this quantises once, so the two can disagree by
// a code or two inside the ramps. The pixel diff in `docs/backdrop-blur-profiling.md` is the
// measurement, not a promise.
//
// Pairs with vs_img.vert, which already hands over v_cuv and v_p (quad-local pixels). u_org is the
// quad's CENTRE in authored pixels, so v_p + u_org is the screen position both fields are defined
// in — the art quad is a `Rect::cover`, so it is not the panel and its own UV cannot stand in.
// v_p/u_org are highp for the same reason fs_src.frag's coordinate chain is: an fp16 difference of
// two ~1000px numbers is a third of a pixel, and these ramps run over hundreds of them.
precision mediump float;
varying highp vec2 v_cuv;
varying highp vec2 v_p;
uniform sampler2D u_tex;
uniform vec4 u_tint;        // art tint; alpha already carries the painter's cascade
uniform highp vec2 u_org;   // the quad's centre, in authored pixels
uniform vec4 u_ink;         // scrim ink (rgb; a unused), already carrying the painter's rgb gain
uniform highp vec4 u_ramp;  // (y0, 1/(knee-y0), knee, 1/(H-knee))
uniform vec2 u_rampa;       // (alpha at the knee, alpha at the foot MINUS the knee's)
uniform highp vec4 u_wedge; // (peak alpha, 1/width, feather top, 1/(feather knee - top))
void main(){
  highp vec2 s = v_p + u_org;
  // the atmospheric ramp: 0 above y0, linear to `mid` at the knee, linear to `mid + d` at the foot
  float t0 = clamp((s.y - u_ramp.x) * u_ramp.y, 0.0, 1.0);
  float t1 = clamp((s.y - u_ramp.z) * u_ramp.w, 0.0, 1.0);
  float a1 = u_rampa.x * t0 + u_rampa.y * t1;
  // the corner wedge: peak at the left margin, gone by its width, feathered in from its top
  float u = clamp(s.x * u_wedge.y, 0.0, 1.0);
  float v = clamp((s.y - u_wedge.z) * u_wedge.w, 0.0, 1.0);
  float a2 = u_wedge.x * (1.0 - u) * v;
  vec4 c = texture2D(u_tex, v_cuv);
  float A = c.a * u_tint.a;
  float B = a1 + a2 - a1 * a2;
  float sa = 1.0 - (1.0 - A) * (1.0 - B);
  vec3 rgb = (c.rgb * u_tint.rgb * (A * (1.0 - B)) + u_ink.rgb * B) / max(sa, 1.0 / 4096.0);
  gl_FragColor = vec4(rgb, sa);
}
