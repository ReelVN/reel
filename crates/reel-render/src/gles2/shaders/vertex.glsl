#version 100
precision highp float;
attribute vec2 a_position,a_uv;
attribute vec4 a_color,a_window;
varying vec2 v_uv;
varying vec4 v_color,v_window;
void main(){gl_Position=vec4(a_position,0.0,1.0);v_uv=a_uv;v_color=a_color;v_window=a_window;}
