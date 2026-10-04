// THE FLAT FILL — one colour, square corners, no focus, no rim, no pill. `gfx::draw_rect` and
// `gfx::draw_rrect` route a uniform rect here instead of through `fs_src.frag`'s flat path, which
// still interpolates a top/bottom pair per fragment for a quad whose two colours are the same.
//
// It exists for ONE surface class: the full-screen scrim. A modal scrim, a popover's dim, the
// exit alert's ground are each 2.07M fragments of one colour, and the mix + varying they paid in
// `fs_src.frag` is ~2 shader words a fragment that produce the uniform they were handed. On the
// set that is ~2M GPU cycles of an alert frame's budget for nothing (2026-09-04).
//
// The vertex half is `vs_src.vert`, so `u_rect`/`u_screen` mean what they mean everywhere else.
precision mediump float;
uniform vec4 u_col;
void main(){
  gl_FragColor = u_col;
}
