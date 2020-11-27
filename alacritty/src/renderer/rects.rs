use std::collections::HashMap;
use std::mem::size_of;

use crossfont::Metrics;

use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Rgb;
use alacritty_terminal::term::{RenderableCell, SizeInfo};

use crate::gl;
use crate::gl::types::*;
use crate::renderer;

#[derive(Debug, Copy, Clone)]
pub struct RenderRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Rgb,
    pub alpha: f32,
}

impl RenderRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32, color: Rgb, alpha: f32) -> Self {
        RenderRect { x, y, width, height, color, alpha }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RenderLine {
    pub start: Point,
    pub end: Point,
    pub color: Rgb,
}

impl RenderLine {
    pub fn rects(&self, flag: Flags, metrics: &Metrics, size: &SizeInfo) -> Vec<RenderRect> {
        let mut rects = Vec::new();

        let mut start = self.start;
        while start.line < self.end.line {
            let end = Point::new(start.line, size.cols() - 1);
            Self::push_rects(&mut rects, metrics, size, flag, start, end, self.color);
            start = Point::new(start.line + 1, Column(0));
        }
        Self::push_rects(&mut rects, metrics, size, flag, start, self.end, self.color);

        rects
    }

    /// Push all rects required to draw the cell's line.
    fn push_rects(
        rects: &mut Vec<RenderRect>,
        metrics: &Metrics,
        size: &SizeInfo,
        flag: Flags,
        start: Point,
        end: Point,
        color: Rgb,
    ) {
        let (position, thickness) = match flag {
            Flags::DOUBLE_UNDERLINE => {
                // Position underlines so each one has 50% of descent available.
                let top_pos = 0.25 * metrics.descent;
                let bottom_pos = 0.75 * metrics.descent;

                rects.push(Self::create_rect(
                    size,
                    metrics.descent,
                    start,
                    end,
                    top_pos,
                    metrics.underline_thickness,
                    color,
                ));

                (bottom_pos, metrics.underline_thickness)
            },
            Flags::UNDERLINE => (metrics.underline_position, metrics.underline_thickness),
            Flags::STRIKEOUT => (metrics.strikeout_position, metrics.strikeout_thickness),
            _ => unimplemented!("Invalid flag for cell line drawing specified"),
        };

        rects.push(Self::create_rect(
            size,
            metrics.descent,
            start,
            end,
            position,
            thickness,
            color,
        ));
    }

    /// Create a line's rect at a position relative to the baseline.
    fn create_rect(
        size: &SizeInfo,
        descent: f32,
        start: Point,
        end: Point,
        position: f32,
        mut thickness: f32,
        color: Rgb,
    ) -> RenderRect {
        let start_x = start.col.0 as f32 * size.cell_width();
        let end_x = (end.col.0 + 1) as f32 * size.cell_width();
        let width = end_x - start_x;

        // Make sure lines are always visible.
        thickness = thickness.max(1.);

        let line_bottom = (start.line.0 as f32 + 1.) * size.cell_height();
        let baseline = line_bottom + descent;

        let mut y = (baseline - position - thickness / 2.).ceil();
        let max_y = line_bottom - thickness;
        if y > max_y {
            y = max_y;
        }

        RenderRect::new(
            start_x + size.padding_x(),
            y + size.padding_y(),
            width,
            thickness,
            color,
            1.,
        )
    }
}

/// Lines for underline and strikeout.
#[derive(Default)]
pub struct RenderLines {
    inner: HashMap<Flags, Vec<RenderLine>>,
}

impl RenderLines {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn rects(&self, metrics: &Metrics, size: &SizeInfo) -> Vec<RenderRect> {
        self.inner
            .iter()
            .flat_map(|(flag, lines)| {
                lines.iter().flat_map(move |line| line.rects(*flag, metrics, size))
            })
            .collect()
    }

    /// Update the stored lines with the next cell info.
    #[inline]
    pub fn update(&mut self, cell: &RenderableCell) {
        self.update_flag(&cell, Flags::UNDERLINE);
        self.update_flag(&cell, Flags::DOUBLE_UNDERLINE);
        self.update_flag(&cell, Flags::STRIKEOUT);
    }

    /// Update the lines for a specific flag.
    fn update_flag(&mut self, cell: &RenderableCell, flag: Flags) {
        if !cell.flags.contains(flag) {
            return;
        }

        // Include wide char spacer if the current cell is a wide char.
        let mut end: Point = cell.into();
        if cell.flags.contains(Flags::WIDE_CHAR) {
            end.col += 1;
        }

        // Check if there's an active line.
        if let Some(line) = self.inner.get_mut(&flag).and_then(|lines| lines.last_mut()) {
            if cell.fg == line.color
                && cell.column == line.end.col + 1
                && cell.line == line.end.line
            {
                // Update the length of the line.
                line.end = end;
                return;
            }
        }

        // Start new line if there currently is none.
        let line = RenderLine { start: cell.into(), end, color: cell.fg };
        match self.inner.get_mut(&flag) {
            Some(lines) => lines.push(line),
            None => {
                self.inner.insert(flag, vec![line]);
            },
        }
    }
}

/// Shader sources for rect rendering program.
pub static RECT_SHADER_F_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/res/rect.f.glsl");
pub static RECT_SHADER_V_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/res/rect.v.glsl");
static RECT_SHADER_F: &str = include_str!("../../res/rect.f.glsl");
static RECT_SHADER_V: &str = include_str!("../../res/rect.v.glsl");

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

/// Struct that stores vertex 2D coordinates and color for rect rendering.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Vertex {
    // Normalized screen coordinates.
    // TODO these can certainly be i16.
    x: f32,
    y: f32,

    // Color.
    color: Rgba,
}

/// Struct to group together rect-related GL objects and rendering functionality.
#[derive(Debug)]
pub struct RectRenderer {
    // GL buffer objects. VAO stores vertex attributes binding.
    vao: GLuint,
    vbo: GLuint,

    program: RectShaderProgram,
}

impl RectRenderer {
    /// Update program when doing live-shader-reload.
    pub fn set_program(&mut self, program: RectShaderProgram) {
        self.program = program;
    }

    pub fn new() -> Result<Self, renderer::Error> {
        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;
        let program = RectShaderProgram::new()?;

        unsafe {
            // Allocate buffers.
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);

            // VBO binding is not part ot VAO itself, but VBO binding is stored in attributes.
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            let mut index = 0;
            let mut size = 0;

            macro_rules! add_attr {
                ($count:expr, $gl_type:expr, $normalize:expr, $type:ty) => {
                    gl::VertexAttribPointer(
                        index,
                        $count,
                        $gl_type,
                        $normalize,
                        size_of::<Vertex>() as i32,
                        size as *const _,
                    );
                    gl::EnableVertexAttribArray(index);

                    #[allow(unused_assignments)]
                    {
                        size += $count * size_of::<$type>();
                        index += 1;
                    }
                };
            }

            // Position.
            add_attr!(2, gl::FLOAT, gl::FALSE, f32);

            // Color.
            add_attr!(4, gl::UNSIGNED_BYTE, gl::TRUE, u8);

            // Reset buffer bindings.
            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }

        Ok(Self { vao, vbo, program })
    }

    pub fn draw(&mut self, size_info: &SizeInfo, rects: Vec<RenderRect>) {
        unsafe {
            // Bind VAO to enable vertex attribute slots specified in new().
            gl::BindVertexArray(self.vao);

            // Bind VBO only once for buffer data upload only.
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);

            gl::UseProgram(self.program.id);
        }

        let center_x = size_info.width() / 2.;
        let center_y = size_info.height() / 2.;

        // Build rect vertices vector.
        let mut vertices = RectVertices::new(rects.len());
        for rect in &rects {
            vertices.add_rect(center_x, center_y, rect);
        }

        unsafe {
            // Upload and render accumulated vertices.
            vertices.draw();

            // Disable program.
            gl::UseProgram(0);

            // Reset buffer bindings to nothing.
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
        }
    }
}

/// Helper struct to hold transient vertices for rendering.
struct RectVertices {
    vertices: Vec<Vertex>,
}

impl RectVertices {
    fn new(rects: usize) -> Self {
        let mut vertices = Vec::new();
        vertices.reserve(rects * 6);
        Self { vertices }
    }

    fn add_rect(&mut self, center_x: f32, center_y: f32, rect: &RenderRect) {
        // Calculate rectangle position.
        let x = (rect.x - center_x) / center_x;
        let y = -(rect.y - center_y) / center_y;
        let width = rect.width / center_x;
        let height = rect.height / center_y;
        let color = Rgba {
            r: rect.color.r,
            g: rect.color.g,
            b: rect.color.b,
            a: (rect.alpha * 255.) as u8,
        };

        // Make quad vertices.
        let quad = [
            Vertex { x, y, color },
            Vertex { x, y: y - height, color },
            Vertex { x: x + width, y, color },
            Vertex { x: x + width, y: y - height, color },
        ];

        // Append the vertices to form two triangles.
        self.vertices.push(quad[0]);
        self.vertices.push(quad[1]);
        self.vertices.push(quad[2]);
        self.vertices.push(quad[2]);
        self.vertices.push(quad[3]);
        self.vertices.push(quad[1]);
    }

    unsafe fn draw(&self) {
        // Upload accumulated vertices.
        gl::BufferData(
            gl::ARRAY_BUFFER,
            (self.vertices.len() * std::mem::size_of::<Vertex>()) as isize,
            self.vertices.as_ptr() as *const _,
            gl::STREAM_DRAW,
        );

        // Draw all vertices as list of triangles.
        gl::DrawArrays(gl::TRIANGLES, 0, self.vertices.len() as i32);
    }
}

/// Rectangle drawing program.
#[derive(Debug)]
pub struct RectShaderProgram {
    /// Program id.
    id: GLuint,
}

impl RectShaderProgram {
    pub fn new() -> Result<Self, renderer::ShaderCreationError> {
        let (vertex_src, fragment_src) = if cfg!(feature = "live-shader-reload") {
            (None, None)
        } else {
            (Some(RECT_SHADER_V), Some(RECT_SHADER_F))
        };
        let vertex_shader =
            renderer::create_shader(RECT_SHADER_V_PATH, gl::VERTEX_SHADER, vertex_src)?;
        let fragment_shader =
            renderer::create_shader(RECT_SHADER_F_PATH, gl::FRAGMENT_SHADER, fragment_src)?;
        let program = renderer::create_program(vertex_shader, fragment_shader)?;

        unsafe {
            gl::DeleteShader(fragment_shader);
            gl::DeleteShader(vertex_shader);
            gl::UseProgram(program);
        }

        let shader = Self { id: program };

        unsafe { gl::UseProgram(0) }

        Ok(shader)
    }
}

impl Drop for RectShaderProgram {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteProgram(self.id);
        }
    }
}
