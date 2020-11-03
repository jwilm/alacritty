#version 300 es
layout (location = 0) in vec2 aPos;
layout (location = 1) in vec2 aUv;
layout (location = 2) in vec3 aFg;
layout (location = 3) in float aFlags;

smooth out vec2 uv;
flat out vec3 fg;
flat out float flags;

uniform vec2 u_scale;

void main()
{
    uv = aUv;
    fg = aFg;
		flags = aFlags;
		vec2 pos = vec2(-1., 1.) + aPos.xy * u_scale;
    gl_Position = vec4(pos, 0.0, 1.0);
}
