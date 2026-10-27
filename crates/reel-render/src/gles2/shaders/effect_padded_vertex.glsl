#version 100
precision highp float;
attribute vec2 a_position,a_uv;
attribute vec4 a_color;
varying vec2 v_uv,reel_implicit_source_uv;
void main(){
 gl_Position=vec4(a_position,0.0,1.0);
 v_uv=a_color.xy;
 reel_implicit_source_uv=vec2(a_uv.x,1.0-a_uv.y);
}
