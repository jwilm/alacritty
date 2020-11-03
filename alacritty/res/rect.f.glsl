#version 300 es
precision mediump float;

flat in vec4 color;

out vec4 FragColor;

void main()
{
    FragColor = color;
}
