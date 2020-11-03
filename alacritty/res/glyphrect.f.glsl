#version 300 es
precision mediump float;

uniform sampler2D u_atlas;

smooth in vec2 uv;
flat in vec3 fg;
flat in float flags;

out vec4 FragColor;

void main() {
		//FragColor = vec4(uv,0.,.4); return;
		vec4 mask = texture(u_atlas, uv);
		bool colored = flags > 0.;
		if (colored) {
			if (mask.a > 0.) {
				mask.rgb /= mask.a;
				FragColor = mask;
			}
		} else {
			FragColor = vec4(fg, mask.r);
		}
}
