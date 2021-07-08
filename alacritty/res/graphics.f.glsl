#version 330 core

// Index in the textures[] uniform.
flat in int texId;

// Texture coordinates.
in vec2 texCoords;

// Array with graphics data.
uniform sampler2D textures[16];

// Computed color.
out vec4 color;

void main() {
    // The expression `textures[texId]` can't be used in OpenGL 3.3.
    // If we try to use it, the compiler throws this error:
    //
    //     sampler arrays indexed with non-constant expressions
    //     are forbidden in GLSL 1.30 and later
    //
    // To overcome this limitation we use a switch for every valid
    // value of `texId`.
    //
    // The first expression (`textures[texId]`) works with OpenGL 4.0
    // or later (using `#version 400 core`). If Alacritty drops support
    // for OpenGL 3.3, this switch block can be replaced with it.


#define TEX(N) case N: color = texture(textures[N], texCoords); break;

    switch(texId) {
        TEX( 0) TEX( 1) TEX( 2) TEX( 3)
        TEX( 4) TEX( 5) TEX( 6) TEX( 7)
        TEX( 8) TEX( 9) TEX(10) TEX(11)
        TEX(12) TEX(13) TEX(14) TEX(15)
        default:
            discard;
    }
}
