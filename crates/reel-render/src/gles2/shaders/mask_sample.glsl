#version 100
precision highp float;
uniform sampler2D u_image;
uniform vec4 u_texels,u_uv0,u_uv1,u_values,u_flags,u_coordinates;
uniform vec2 u_dimensions,u_output_size,u_sample_flags;
#ifdef REEL_MASK_COMPOSITE
varying vec2 v_uv;
uniform sampler2D u_source;
uniform vec4 u_params;
#endif
#ifndef REEL_MASK_THRESHOLD
uniform vec4 u_step0,u_step1,u_step2,u_step3,u_step4,u_step5,u_step6,u_step7;
float chain_step(float value,vec4 operation){
 if(operation.x==0.0)return value;
 if(operation.x==1.0)return value+operation.y;
 if(operation.x==2.0)return value-operation.y;
 if(operation.x==3.0)return operation.y-value;
 if(operation.x==4.0)return value*operation.y;
 if(operation.x==5.0)return clamp(value,operation.y,operation.z);
 return value;
}
#endif
vec3 encoded(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
float mask_coverage(){
 vec3 position=vec3(gl_FragCoord.x,u_output_size.y-gl_FragCoord.y,1.0);
 vec2 uv=vec2(dot(u_uv0.xyz,position),dot(u_uv1.xyz,position));
 if(any(lessThan(uv,vec2(0.0)))||any(greaterThan(uv,vec2(1.0))))return 0.0;
#ifdef REEL_MASK_THRESHOLD
 if(u_values.x<=0.0)return 0.0;
 if(u_values.x>=1.0)return 1.0;
#endif
 vec2 at=uv*u_coordinates.zw+u_coordinates.xy;
 if(mod(u_sample_flags.y,2.0)>0.5)at.x=fract(at.x);
 if(u_sample_flags.y>1.5)at.y=fract(at.y);
 vec2 half_texel=0.5/u_dimensions;
 at=clamp(at*u_texels.xy,half_texel,u_texels.xy-half_texel);
 vec4 color=texture2D(u_image,at);
 if(u_texels.w==1.0)color.a=1.0;
 if(u_texels.w==2.0)color.rgb=min(color.rgb,vec3(color.a));
 float value=color.a;
#ifndef REEL_MASK_THRESHOLD
 if(u_sample_flags.x>0.5){
#endif
  vec3 rgb=color.a>0.0?clamp(color.rgb/color.a,0.0,1.0):vec3(0.0);
  if(u_texels.z==0.0)rgb=encoded(rgb);
  value=dot(rgb,vec3(0.2126,0.7152,0.0722));
#ifndef REEL_MASK_THRESHOLD
 }
 value=chain_step(value,u_step0);
 value=chain_step(value,u_step1);
 value=chain_step(value,u_step2);
 value=chain_step(value,u_step3);
 value=chain_step(value,u_step4);
 value=chain_step(value,u_step5);
 value=chain_step(value,u_step6);
 value=chain_step(value,u_step7);
#else
 if(u_flags.x!=0.0)value=1.0-value;
 float threshold=step(value,u_values.x);
 if(u_values.y>0.0)threshold=clamp((u_values.x*(1.0+u_values.y)-value)/u_values.y,0.0,1.0);
 value=color.a*threshold;
#endif
 return clamp(value,0.0,1.0);
}
void main(){
#ifdef REEL_MASK_COMPOSITE
 // Keep coverage in highp until the final linear-premultiplied blend, as on WGPU.
 gl_FragColor=texture2D(u_source,vec2(v_uv.x,1.0-v_uv.y))*(u_params.x*mask_coverage());
#else
 gl_FragColor=vec4(mask_coverage());
#endif
}
