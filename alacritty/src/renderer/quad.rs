use super::atlas::{Atlas, AtlasInsertError};
use super::glyph::{QuadAtlasGlyph, RasterizedGlyph};
use super::math::*;
use super::shade::GlyphRectShaderProgram;
use crate::gl;
use crate::gl::types::*;
use crate::renderer::Error;
use alacritty_terminal::term::SizeInfo;

use log::*;

use std::mem::size_of;
use std::ptr;

enum RectAddError {
    Full,
}

pub struct GlyphQuad<'a> {
    pub glyph: &'a QuadAtlasGlyph,
    pub pos: Vec2<i16>,
    pub fg: alacritty_terminal::term::color::Rgb,
}

#[derive(Debug)]
pub struct QuadGlyphRenderer {
    atlas_groups: Vec<AtlasGroup>,

    // GL objects for shared use. There's no point in having these per atlas/batch, as their
    // content is completely transient currently.
    program: GlyphRectShaderProgram,
    vao: GLuint,
    vbo: GLuint,
    ebo: GLuint,
}

impl QuadGlyphRenderer {
    pub fn new() -> Self {
        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;
        let mut ebo: GLuint = 0;

        // Pre-generate indices once.
        // TODO there should be a solution using flat_map, but I failed to find one
        let indices = {
            let mut indices = Vec::<u16>::new();
            for index in 0 as u16..(65536 / 4) as u16 {
                let i = index * 4;
                indices.push(i);
                indices.push(i + 1);
                indices.push(i + 2);

                indices.push(i + 2);
                indices.push(i + 3);
                indices.push(i + 1);
            }
            indices
        };

        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);
            gl::GenBuffers(1, &mut ebo);

            // Set up VAO bindings.
            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            // Position.
            gl::VertexAttribPointer(
                0,
                2,
                gl::SHORT,
                gl::FALSE,
                (size_of::<Vertex>()) as _,
                ptr::null(),
            );
            gl::EnableVertexAttribArray(0);

            // uv.
            gl::VertexAttribPointer(
                1,
                2,
                gl::FLOAT,
                gl::FALSE,
                (size_of::<Vertex>()) as _,
                offset_of!(Vertex, u) as *const _,
            );
            gl::EnableVertexAttribArray(1);

            // Foreground color.
            gl::VertexAttribPointer(
                2,
                3,
                gl::UNSIGNED_BYTE,
                gl::TRUE,
                (size_of::<Vertex>()) as _,
                offset_of!(Vertex, fg) as *const _,
            );
            gl::EnableVertexAttribArray(2);

            // Flags.
            gl::VertexAttribPointer(
                3,
                1,
                gl::UNSIGNED_BYTE,
                gl::FALSE,
                (size_of::<Vertex>()) as _,
                offset_of!(Vertex, flags) as *const _,
            );
            gl::EnableVertexAttribArray(3);

            // Pre-upload indices.
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, ebo);
            gl::BufferData(
                gl::ELEMENT_ARRAY_BUFFER,
                (indices.len() * std::mem::size_of::<u16>()) as isize,
                indices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );
        }
        Self {
            vao,
            vbo,
            ebo,
            atlas_groups: Vec::new(),
            program: GlyphRectShaderProgram::new().unwrap(),
        }
    }

    pub fn clear_atlas(&mut self) {
        for group in &mut self.atlas_groups {
            group.clear_atlas();
        }
    }

    pub fn clear(&mut self) {
        for group in &mut self.atlas_groups {
            group.clear();
        }
    }

    pub fn insert_into_atlas(&mut self, rasterized: &RasterizedGlyph) -> QuadAtlasGlyph {
        loop {
            for group in &mut self.atlas_groups {
                match group.atlas.insert(rasterized) {
                    Ok(glyph) => {
                        return glyph;
                    },
                    Err(AtlasInsertError::GlyphTooLarge) => {
                        error!("Glyph for char {:x} is too large", rasterized.rasterized.c as u32);
                        return QuadAtlasGlyph {
                            atlas_index: 0,
                            colored: false,
                            uv_bot: 0.,
                            uv_left: 0.,
                            uv_width: 0.,
                            uv_height: 0.,
                            top: 0,
                            left: 0,
                            width: 0,
                            height: 0,
                        };
                    },
                    Err(AtlasInsertError::Full) => {},
                }
            }

            self.atlas_groups.push(AtlasGroup::new(self.atlas_groups.len()));
        }
    }

    pub fn add_to_render(&mut self, size_info: &SizeInfo, glyph: &GlyphQuad<'_>) {
        self.atlas_groups[glyph.glyph.atlas_index].add(size_info, glyph);
    }

    pub fn draw(&mut self, size_info: &SizeInfo) {
        #[cfg(feature = "live-shader-reload")]
        {
            match self.program.poll() {
                Err(e) => {
                    error!("shader error: {}", e);
                },
                Ok(updated) if updated => {
                    debug!("updated shader: {:?}", self.program);
                },
                _ => {},
            }
        }

        // Swap to rectangle rendering program.
        unsafe {
            // Add padding to viewport.
            let pad_x = size_info.padding_x() as i32;
            let pad_y = size_info.padding_y() as i32;
            let width = size_info.width() as i32 - 2 * pad_x;
            let height = size_info.height() as i32 - 2 * pad_y;
            gl::Viewport(pad_x, pad_y, width, height);

            // Swap program.
            gl::UseProgram(self.program.get_id());

            gl::Uniform1i(self.program.u_atlas, 0);
            gl::Uniform2f(self.program.u_scale, 2.0 / width as f32, -2.0 / height as f32);

            // Change blending strategy.
            gl::Enable(gl::BLEND);
            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);

            // Set VAO bindings.
            gl::BindVertexArray(self.vao);

            // VBO is not part of VAO state. VBO binding will be used for uploading vertex data.
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
        }

        for group in &mut self.atlas_groups {
            group.draw();
        }
    }
}

#[derive(Debug)]
struct AtlasGroup {
    atlas: Atlas,
    batches: Vec<Batch>,
}

impl AtlasGroup {
    fn new(index: usize) -> Self {
        Self { atlas: Atlas::new(index, 1024), batches: Vec::new() }
    }

    fn clear_atlas(&mut self) {
        self.atlas.clear();
    }

    fn clear(&mut self) {
        for batch in &mut self.batches {
            batch.clear();
        }
    }

    fn add(&mut self, size_info: &SizeInfo, glyph_rect: &GlyphQuad<'_>) {
        loop {
            if !self.batches.is_empty() {
                match self.batches.last_mut().unwrap().add(size_info, glyph_rect) {
                    Ok(_) => {
                        return;
                    },
                    Err(RectAddError::Full) => {},
                }
            }

            self.batches.push(Batch::new().unwrap());
        }
    }

    fn draw(&mut self) {
        unsafe {
            // Binding to active slot 0
            gl::BindTexture(gl::TEXTURE_2D, self.atlas.id);
        }

        for batch in &mut self.batches {
            batch.draw();
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

impl Rgb {
    fn from(color: alacritty_terminal::term::color::Rgb) -> Rgb {
        Rgb { r: color.r, g: color.g, b: color.b }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Vertex {
    x: i16,
    y: i16,
    // TODO these can also be u/i16
    u: f32,
    v: f32,
    fg: Rgb,
    flags: u8,
}

#[derive(Debug)]
struct Batch {
    vertices: Vec<Vertex>,
}

impl Batch {
    fn new() -> Result<Self, Error> {
        Ok(Self { vertices: Vec::new() })
    }

    fn clear(&mut self) {
        self.vertices.clear();
    }

    fn add(&mut self, size_info: &SizeInfo, glyph: &GlyphQuad<'_>) -> Result<(), RectAddError> {
        let index = self.vertices.len();
        if index >= 65536 - 4 {
            return Err(RectAddError::Full);
        }

        let g = glyph.glyph;

        // Calculate rectangle position.
        let x = glyph.pos.x + g.left;
        let y = glyph.pos.y + (size_info.cell_height() as i16 - g.top);
        let fg = Rgb::from(glyph.fg);
        let flags = if g.colored { 1 } else { 0 };

        self.vertices.push(Vertex {
            x,
            y: y + g.height,
            u: g.uv_left,
            v: g.uv_bot + g.uv_height,
            fg,
            flags,
        });
        self.vertices.push(Vertex { x, y, u: g.uv_left, v: g.uv_bot, fg, flags });
        self.vertices.push(Vertex {
            x: x + g.width,
            y: y + g.height,
            u: g.uv_left + g.uv_width,
            v: g.uv_bot + g.uv_height,
            fg,
            flags,
        });
        self.vertices.push(Vertex {
            x: x + g.width,
            y,
            u: g.uv_left + g.uv_width,
            v: g.uv_bot,
            fg,
            flags,
        });

        Ok(())
    }

    fn draw(&mut self) {
        if self.vertices.is_empty() {
            return;
        }

        unsafe {
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertices.len() * std::mem::size_of::<Vertex>()) as isize,
                self.vertices.as_ptr() as *const _,
                gl::STREAM_DRAW,
            );

            gl::DrawElements(
                gl::TRIANGLES,
                (self.vertices.len() / 4 * 6) as i32,
                gl::UNSIGNED_SHORT,
                ptr::null(),
            );
        }
    }
}
