// Text fragment shader: tints the SDL2_ttf-rendered string texture's alpha with u_tcol. v_tuv
// must be highp: string textures are sampled 1:1 with GL_LINEAR and long lines reach 660-1300px
// wide - Midgard's fp16 varying interpolation is off by up to ~0.5 texel at that width, unevenly
// blurring 1px glyph stems along the line (see the PRECISION note in fs_src.frag).
precision mediump float;
varying highp vec2 v_tuv;
uniform sampler2D u_tex;
uniform vec4 u_tcol;
void main(){ float a=texture2D(u_tex,v_tuv).a; gl_FragColor=vec4(u_tcol.rgb, u_tcol.a*a); }
