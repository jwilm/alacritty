/* Version directive added by shader_composer() */
layout (location = 0) in vec2 aPos;
layout (location = 1) in vec4 aColor;

flat out vec4 color;

void main()
{
    color = aColor;
    gl_Position = vec4(aPos.x, aPos.y, 0.0, 1.0);
}
