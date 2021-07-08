#version 330 core

// ------
// INPUTS

// Texture associated to the graphic.
layout(location = 0) in int textureId;

// Sides where the vertex is located.
//
// Bit 0 (LSB) is 0 for top and 1 for bottom.
// Bit 1 is 0 for left and 1 for right.
layout(location = 1) in int sides;

// Column number in the grid where the left vertex is set.
layout(location = 2) in float column;

// Line number in the grid where the left vertex is set.
layout(location = 3) in float line;

// Height in pixels of the texture.
layout(location = 4) in float height;

// Width in pixels of the texture.
layout(location = 5) in float width;

// Offset in the X direction.
layout(location = 6) in float offsetX;

// Offset in the Y direction.
layout(location = 7) in float offsetY;

// Height in pixels of a single cell when the graphic was added.
layout(location = 8) in float baseCellHeight;

// -------
// OUTPUTS

// Texture sent to the fragment shader.
flat out int texId;

// Coordinates sent to the fragment shader.
out vec2 texCoords;

// --------
// UNIFORMS

// Width and height of a single cell.
uniform vec2 cellDimensions;

// Width and height of the view.
uniform vec2 viewDimensions;


#define IS_RIGHT_SIDE ((sides & 1) == 1)
#define IS_BOTTOM_SIDE ((sides & 2) == 2)

void main() {
    float scale = cellDimensions.y / baseCellHeight;
    float x = (column * cellDimensions.x - offsetX * scale) / (viewDimensions.x / 2) - 1;
    float y = -(line * cellDimensions.y - offsetY * scale) / (viewDimensions.y / 2) + 1;

    vec4 position = vec4(x, y, 0, 1);
    vec2 coords = vec2(0, 0);

    if(IS_RIGHT_SIDE) {
        position.x += scale * width / (viewDimensions.x / 2);
        coords.x = 1;
    }

    if(IS_BOTTOM_SIDE) {
        position.y += -scale * height / (viewDimensions.y / 2);
        coords.y = 1;
    }

    gl_Position = position;
    texCoords = coords;
    texId = textureId;
}
