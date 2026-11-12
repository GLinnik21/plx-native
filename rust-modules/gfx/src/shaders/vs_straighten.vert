// The STRAIGHTEN pass's vertex shader (`gfx/straighten.rs`): the shared unit quad, stretched over ONE
// box of the viewport. The pass draws a quad over each box the frame drew into and over nothing else
// (a pixel no draw touched is the transparent clear, which the pass leaves as it is), so the quad is
// not the viewport: `u_box` is the box as the fractions `(x0, y0, x1, y1)` of the viewport, `0, 0` its
// bottom-left, and `v_uv` is the pixel's place in the WHOLE viewport, which is all the COPY variant
// (`fs_straighten.frag` without a fetch extension) needs to read its own copy back 1:1.
//
// `mix` lands exactly on the box's edges at the unit quad's corners (0 and 1), so two boxes that touch
// compute the same position for the edge they share, and the rasterizer's fill rule gives each pixel
// of it to exactly one of them: the fetch program must not visit a pixel twice. Vertex shaders are
// highp on every GPU this runs on; the edges are whole pixels over the viewport's size, half a pixel
// from the nearest pixel centre.
attribute vec2 a_pos;
uniform highp vec4 u_box;
varying highp vec2 v_uv;
void main(){
  v_uv = mix(u_box.xy, u_box.zw, a_pos);
  gl_Position = vec4(v_uv * 2.0 - 1.0, 0.0, 1.0);
}
