use std::mem;

use crate::gl;
use crate::gl::types::*;
use crate::renderer;

/// Number of elements of the `textures[]` uniform.
///
/// If the file `graphics.f.glsl` is modified, this value has to be updated.
pub(super) const TEXTURES_ARRAY_SIZE: usize = 16;

/// Sides where the vertex is located.
///
/// * Bit 0 (LSB) is 0 for top and 1 for bottom.
/// * Bit 1 is 0 for left and 1 for right.
#[derive(Debug, Copy, Clone, PartialEq)]
#[repr(u8)]
pub enum VertexSide {
    TopLeft = 0b00,
    TopRight = 0b10,
    BottomLeft = 0b01,
    BottomRight = 0b11,
}

/// Vertex data to execute the graphics rendering program.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Vertex {
    /// Texture associated to the graphic.
    pub texture_id: GLuint,

    /// Sides where the vertex is located.
    pub sides: VertexSide,

    /// Column number in the grid where the left vertex is set.
    pub column: GLuint,

    /// Line where the top vertex is set.
    pub line: GLuint,

    /// Height, in pixels, of the texture.
    pub height: u16,

    /// Width, in pixels, of the texture.
    pub width: u16,

    /// Offset in the x direction.
    pub offset_x: u16,

    /// Offset in the y direction.
    pub offset_y: u16,

    /// Height, in pixels, of a single cell when the graphic was added.
    pub base_cell_height: f32,
}

/// Sources for the graphics rendering program.
static GRAPHICS_SHADER_F: &str = include_str!("../../../res/graphics.f.glsl");
static GRAPHICS_SHADER_V: &str = include_str!("../../../res/graphics.v.glsl");

/// Graphics rendering program.
#[derive(Debug)]
pub struct GraphicsShaderProgram {
    /// Program id.
    pub id: GLuint,

    /// Uniform of the cell dimensions.
    pub u_cell_dimensions: GLint,

    /// Uniform of the view dimensions.
    pub u_view_dimensions: GLint,

    /// Uniform array of the textures.
    pub u_textures: Vec<GLint>,

    /// Vertex Array Object (VAO) for the fields of `Vertex`.
    pub vao: GLuint,

    /// Vertex Buffer Object (VBO) to send instances of `Vertex`.
    pub vbo: GLuint,
}

impl GraphicsShaderProgram {
    pub fn new() -> Result<Self, renderer::ShaderCreationError> {
        let vertex_shader = renderer::create_shader(gl::VERTEX_SHADER, GRAPHICS_SHADER_V)?;
        let fragment_shader = renderer::create_shader(gl::FRAGMENT_SHADER, GRAPHICS_SHADER_F)?;
        let program = renderer::create_program(vertex_shader, fragment_shader)?;

        let u_cell_dimensions;
        let u_view_dimensions;
        let u_textures;

        unsafe {
            gl::DeleteShader(fragment_shader);
            gl::DeleteShader(vertex_shader);

            gl::UseProgram(program);

            // Uniform locations.

            macro_rules! uniform {
                ($name:literal) => {
                    gl::GetUniformLocation(
                        program,
                        concat!($name, "\0").as_bytes().as_ptr().cast(),
                    )
                };

                ($fmt:literal, $($arg:tt)+) => {
                    match format!(concat!($fmt, "\0"), $($arg)+) {
                        name => gl::GetUniformLocation(
                            program,
                            name.as_bytes().as_ptr().cast(),
                        )
                    }
                };
            }

            u_cell_dimensions = uniform!("cellDimensions");
            u_view_dimensions = uniform!("viewDimensions");
            u_textures =
                (0..TEXTURES_ARRAY_SIZE).map(|unit| uniform!("textures[{}]", unit)).collect();

            gl::UseProgram(0);
        }

        let (vao, vbo) = define_vertex_attributes();

        let shader =
            Self { id: program, u_cell_dimensions, u_view_dimensions, u_textures, vao, vbo };

        Ok(shader)
    }
}

/// Build a Vertex Array Object (VAO) and a Vertex Buffer Object (VBO) for
/// instances of the `Vertex` type.
fn define_vertex_attributes() -> (GLuint, GLuint) {
    let mut vao = 0;
    let mut vbo = 0;

    unsafe {
        gl::GenVertexArrays(1, &mut vao);
        gl::GenBuffers(1, &mut vbo);

        gl::BindVertexArray(vao);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

        let mut attr_index = 0;

        macro_rules! int_attr {
            ($type:ident, $field:ident) => {
                gl::VertexAttribIPointer(
                    attr_index,
                    1,
                    gl::$type,
                    mem::size_of::<Vertex>() as i32,
                    memoffset::offset_of!(Vertex, $field) as *const _,
                );

                attr_index += 1;
            };
        }

        macro_rules! float_attr {
            ($type:ident, $field:ident) => {
                gl::VertexAttribPointer(
                    attr_index,
                    1,
                    gl::$type,
                    gl::FALSE,
                    mem::size_of::<Vertex>() as i32,
                    memoffset::offset_of!(Vertex, $field) as *const _,
                );

                attr_index += 1;
            };
        }

        int_attr!(UNSIGNED_INT, texture_id);
        int_attr!(UNSIGNED_BYTE, sides);

        float_attr!(UNSIGNED_INT, column);
        float_attr!(UNSIGNED_INT, line);
        float_attr!(UNSIGNED_SHORT, height);
        float_attr!(UNSIGNED_SHORT, width);
        float_attr!(UNSIGNED_SHORT, offset_x);
        float_attr!(UNSIGNED_SHORT, offset_y);
        float_attr!(FLOAT, base_cell_height);

        for index in 0..attr_index {
            gl::EnableVertexAttribArray(index);
        }

        gl::BindVertexArray(0);
        gl::BindBuffer(gl::ARRAY_BUFFER, 0);
    }

    (vao, vbo)
}
