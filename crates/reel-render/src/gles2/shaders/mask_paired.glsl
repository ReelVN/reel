#version 100
precision highp float;
uniform sampler2D u_image0,u_image1;
uniform vec4 u_coordinates[2],u_texels[2],u_dimensions[2];
uniform vec4 u_ops[8];
uniform vec4 u_uv0,u_uv1,u_flags;
uniform vec2 u_output_size;
#ifdef REEL_MASK_COMPOSITE
varying vec2 v_source_uv;
uniform sampler2D u_source;
uniform vec4 u_params;
#endif
vec3 encoded(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
float smooth_mask(float a,float b,float x){if(a==b)return step(a,x);float t=clamp((x-a)/(b-a),0.0,1.0);return t*t*(3.0-2.0*t);}
float joined(float a,float b){if(u_flags.w==1.0)return a+b;if(u_flags.w==2.0)return a-b;return a*b;}
float chain_step(float value,vec4 operation){
 if(operation.x==1.0)return value+operation.y;
 if(operation.x==2.0)return value-operation.y;
 if(operation.x==3.0)return operation.y-value;
 if(operation.x==4.0)return value*operation.y;
 if(operation.x==5.0)return operation.y==0.0?1.0:pow(max(value,0.0),operation.y);
 if(operation.x==6.0)return clamp(value,operation.y,operation.z);
 return smooth_mask(operation.y,operation.z,value);
}
vec2 coordinates(vec2 uv,vec4 c,vec4 texels,vec4 dimensions){
 vec2 at=uv*c.zw+c.xy;
 if(mod(dimensions.z,2.0)>0.5)at.x=fract(at.x);
 if(dimensions.z>1.5)at.y=fract(at.y);
 vec2 half_texel=0.5/dimensions.xy;
 return clamp(at*texels.xy,half_texel,texels.xy-half_texel);
}
float alpha(vec4 color,vec4 texels){return texels.w==1.0?1.0:color.a;}
float gray(vec4 color,vec4 texels){
 if(texels.w==1.0)color.a=1.0;
 if(texels.w==2.0)color.rgb=min(color.rgb,vec3(color.a));
 if(!(color.a>0.0))return 0.0;
 vec3 rgb=clamp(color.rgb/color.a,0.0,1.0);
 if(texels.z==0.0)rgb=encoded(rgb);
 return dot(rgb,vec3(0.2126,0.7152,0.0722));
}
void main(){
 vec3 position=vec3(gl_FragCoord.x,u_output_size.y-gl_FragCoord.y,1.0);
 vec2 uv=vec2(dot(u_uv0.xyz,position),dot(u_uv1.xyz,position));
 if(any(lessThan(uv,vec2(0.0)))||any(greaterThan(uv,vec2(1.0)))){gl_FragColor=vec4(0.0);return;}
 vec4 first=texture2D(u_image0,coordinates(uv,u_coordinates[0],u_texels[0],u_dimensions[0]));
#ifdef REEL_MASK_FIRST_GRAY
 float a=gray(first,u_texels[0]);
#else
 float a=alpha(first,u_texels[0]);
#endif
 vec4 second=texture2D(u_image1,coordinates(uv,u_coordinates[1],u_texels[1],u_dimensions[1]));
#ifdef REEL_MASK_SECOND_GRAY
 float b=gray(second,u_texels[1]);
#else
 float b=alpha(second,u_texels[1]);
#endif
 // A contiguous instruction stream visits the two chains and then their join.
 // Each chain has one scalar accumulator; no fragment register addressing.
 for(int i=0;i<8;i++){
  if(float(i)>=u_flags.z)break;
  if(float(i)<u_flags.x)a=chain_step(a,u_ops[i]);
  else if(float(i)<u_flags.y)b=chain_step(b,u_ops[i]);
  else {if(float(i)==u_flags.y)a=joined(a,b);a=chain_step(a,u_ops[i]);}
 }
 if(u_flags.y==u_flags.z)a=joined(a,b);
 float coverage=clamp(a,0.0,1.0);
#ifdef REEL_MASK_COMPOSITE
 gl_FragColor=texture2D(u_source,v_source_uv)*(u_params.x*coverage);
#else
 gl_FragColor=vec4(coverage);
#endif
}
