#version 100
precision highp float;
varying vec2 v_uv;
uniform sampler2D u_image;
vec3 encode(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
void main(){vec4 color=texture2D(u_image,vec2(v_uv.x,1.0-v_uv.y));gl_FragColor=vec4(encode(color.rgb),1.0);}
