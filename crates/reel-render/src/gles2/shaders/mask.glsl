#version 100
precision highp float;
uniform sampler2D u_image0,u_image1,u_image2,u_image3;
uniform vec4 u_ops[64],u_texels[4],u_dimensions[4];
uniform vec4 u_uv0,u_uv1,u_values,u_flags;
uniform vec2 u_output_size;
uniform int u_count;
vec4 image_sample(int image,vec2 uv){if(image==0)return texture2D(u_image0,uv);if(image==1)return texture2D(u_image1,uv);if(image==2)return texture2D(u_image2,uv);return texture2D(u_image3,uv);}
vec3 encoded(vec3 x){return mix(1.055*pow(max(x,vec3(0.0)),vec3(1.0/2.4))-0.055,12.92*x,step(x,vec3(0.0031308)));}
float smooth_mask(float a,float b,float x){if(a==b)return step(a,x);float t=clamp((x-a)/(b-a),0.0,1.0);return t*t*(3.0-2.0*t);}
void main(){
 vec3 position=vec3(gl_FragCoord.x,u_output_size.y-gl_FragCoord.y,1.0);
 vec2 uv=vec2(dot(u_uv0.xyz,position),dot(u_uv1.xyz,position));
 if(any(lessThan(uv,vec2(0.0)))||any(greaterThan(uv,vec2(1.0)))){gl_FragColor=vec4(0.0);return;}
 float nodes[32];float value=0.0;
 for(int i=0;i<32;i++){
  if(i>=u_count)break;
  vec4 a=u_ops[i*2],b=u_ops[i*2+1];int op=int(a.x);
  if(op==0)value=a.y;
  else if(op==1){if(a.y<0.5)value=u_values.x;else if(a.y<1.5)value=u_values.y;else if(a.y<2.5)value=u_values.z;else value=u_values.w;}
  else if(op==2||op==3){
   int image=int(a.y);vec2 at=uv*vec2(nodes[int(b.x)],nodes[int(b.y)])+vec2(nodes[int(a.z)],nodes[int(a.w)]);
   if(mod(b.z,2.0)>0.5)at.x=fract(at.x);if(b.z>1.5)at.y=fract(at.y);
   vec4 texels=u_texels[image],dimensions=u_dimensions[image];vec2 half_texel=0.5/dimensions.xy;
   at=clamp(at*texels.xy,half_texel,texels.xy-half_texel);
   vec4 color=image_sample(image,at);
   if(texels.w==1.0)color.a=1.0;if(texels.w==2.0)color.rgb=min(color.rgb,vec3(color.a));
   if(op==2)value=color.a;
   else {vec3 rgb=color.a>0.0?clamp(color.rgb/color.a,0.0,1.0):vec3(0.0);if(texels.z==0.0)rgb=encoded(rgb);value=dot(rgb,vec3(0.2126,0.7152,0.0722));}
  }
  else if(op==4)value=nodes[int(a.y)]+nodes[int(a.z)];
  else if(op==5)value=nodes[int(a.y)]-nodes[int(a.z)];
  else if(op==6)value=nodes[int(a.y)]*nodes[int(a.z)];
  else if(op==7)value=a.z==0.0?1.0:pow(max(nodes[int(a.y)],0.0),a.z);
  else if(op==8)value=clamp(nodes[int(a.y)],a.z,a.w);
  else if(op==9)value=smooth_mask(nodes[int(a.y)],nodes[int(a.z)],nodes[int(a.w)]);
  else {float gray=nodes[int(a.z)];if(u_flags.x!=0.0)gray=1.0-gray;float threshold=step(gray,u_values.x);if(u_values.y>0.0)threshold=clamp((u_values.x*(1.0+u_values.y)-gray)/u_values.y,0.0,1.0);value=nodes[int(a.y)]*threshold;if(u_values.x<=0.0)value=0.0;if(u_values.x>=1.0)value=1.0;}
  nodes[i]=value;
 }
 gl_FragColor=vec4(clamp(value,0.0,1.0));
}
