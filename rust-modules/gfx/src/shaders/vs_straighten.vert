// The STRAIGHTEN pass's vertex shader (`gfx/straighten.rs`): the shared unit quad, stretched over the
// whole viewport. Nothing is positioned in authored space here — the pass rewrites every pixel of the
// frame in place, so the quad is the viewport and `v_uv` is the pixel's place in it, which is all the
// COPY variant (`fs_straighten.frag` without a fetch extension) needs to read its own copy back 1:1.
attribute vec2 a_pos;
varying highp vec2 v_uv;
void main(){
  v_uv = a_pos;
  gl_Position = vec4(a_pos * 2.0 - 1.0, 0.0, 1.0);
}
