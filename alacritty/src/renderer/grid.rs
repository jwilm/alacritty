use super::atlas::{AtlasInsertError, GridAtlas};
use super::glyph::{GridAtlasGlyph, RasterizedGlyph};
use super::math::*;
use super::shade::GridShaderProgram;
use super::texture::{create_texture, upload_texture, PixelFormat};
use crate::gl;
use crate::gl::types::*;
use crate::renderer::Error;
use alacritty_terminal::term::{color::Rgb, RenderableCell, SizeInfo};
use log::*;
use std::ptr;

#[derive(Debug)]
pub struct CursorRef {
    atlas_index: usize,
    cell: [f32; 2],
    glyph: [f32; 2],
    color: [f32; 3],
}

#[derive(Debug)]
pub struct GridGlyphRenderer {
    /// Screen size in cells.
    columns: usize,
    lines: usize,

    /// Grid cell metrics in pixels.
    cell_size: Vec2<i32>,
    cell_offset: Vec2<i32>,

    /// Foreground colors array for each cell.
    screen_colors_fg: Vec<[u8; 3]>,

    /// Background colors array for each cell.
    screen_colors_bg: Vec<[u8; 4]>,

    /// Background alpha for empty cells.
    bg_alpha: u8,

    /// Texture that stores glyphs data references for each cell of the screen.
    screen_glyphs_ref_tex: GLuint,

    /// Texture that stores foreground color for each cell.
    screen_colors_fg_tex: GLuint,

    /// Texture that stores background color for each cell.
    screen_colors_bg_tex: GLuint,

    /// Shader program that paints the entire screen.
    program: GridShaderProgram,

    /// Vertex array and buffer objects.
    vao: GLuint,
    vbo: GLuint,

    /// Current cursor data, if any.
    cursor: Option<CursorRef>,

    /// Rendering passes. Potentially need multiple because not all glyphs may fit into a single
    /// atlas texture.
    grid_passes: Vec<GridPass>,
}

impl GridGlyphRenderer {
    pub fn new() -> Result<Self, Error> {
        let screen_glyphs_ref_tex = unsafe { create_texture(256, 256, PixelFormat::RGB8) };
        let screen_colors_fg_tex = unsafe { create_texture(256, 256, PixelFormat::RGBA8) };
        let screen_colors_bg_tex = unsafe { create_texture(256, 256, PixelFormat::RGB8) };

        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;

        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::BindVertexArray(vao);

            // Upload just a single full-screen quad.
            let vertices: [f32; 8] = [-1., 1., -1., -1., 1., 1., 1., -1.];
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                std::mem::size_of_val(&vertices) as isize,
                vertices.as_ptr() as *const _,
                gl::STREAM_DRAW,
            );

            // Set up VAO bindings for future use.
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 0, ptr::null());
            gl::EnableVertexAttribArray(0);
        }

        Ok(Self {
            columns: 0,
            lines: 0,

            cell_size: Vec2 { x: 0, y: 0 },
            cell_offset: Vec2 { x: 0, y: 0 },

            screen_colors_fg: Vec::new(),
            screen_colors_bg: Vec::new(),
            bg_alpha: 255,

            screen_glyphs_ref_tex,
            screen_colors_fg_tex,
            screen_colors_bg_tex,
            program: GridShaderProgram::new()?,
            vao,
            vbo,

            cursor: None,

            grid_passes: Vec::new(),
        })
    }

    /// Resize buffers for a new screen resolution.
    pub fn resize(&mut self, size_info: &SizeInfo) {
        self.columns = size_info.cols().0;
        self.lines = size_info.visible_lines().0;
        let cells = self.columns * self.lines;

        self.screen_colors_bg.resize(cells, [0u8; 4]);
        self.screen_colors_fg.resize(cells, [0u8; 3]);

        for pass in &mut self.grid_passes {
            pass.resize(self.columns, self.lines);
        }
    }

    /// Clear internal buffers to prepare for the next frame.
    pub fn clear(&mut self, color: Rgb, background_opacity: f32) {
        for pass in &mut self.grid_passes {
            pass.clear();
        }

        self.cursor = None;
        let bg_alpha = (background_opacity * 255.0) as u8;
        self.bg_alpha = bg_alpha;
        self.screen_colors_bg.iter_mut().for_each(|x| *x = [color.r, color.g, color.b, bg_alpha]);
        self.screen_colors_fg.iter_mut().for_each(|x| *x = [0u8; 3]);
    }

    /// Completely obliterate atlas data in case e.g. font changed.
    pub fn clear_atlas(&mut self, cell_size: Vec2<i32>, cell_offset: Vec2<i32>) {
        self.cell_size = cell_size;
        self.cell_offset = cell_offset;

        self.grid_passes.clear();
    }

    /// Update cursor coordinates and appearance.
    pub fn set_cursor(
        &mut self,
        atlas_index: usize,
        column: i32,
        line: i32,
        glyph_x: f32,
        glyph_y: f32,
        color: Rgb,
    ) {
        self.cursor = Some(CursorRef {
            atlas_index,
            cell: [column as f32, line as f32],
            glyph: [glyph_x, glyph_y],
            color: [color.r as f32 / 255., color.g as f32 / 255., color.b as f32 / 255.],
        });
        self.grid_passes[atlas_index].dirty = true;
    }

    /// Try to load a new rasterized glyph into grid atlas.
    /// Returns None if glyph cannot be rendered with grid method.
    pub fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Option<GridAtlasGlyph> {
        if rasterized.wide || rasterized.zero_width {
            return None;
        }

        loop {
            if !self.grid_passes.is_empty() {
                match self.grid_passes.last_mut().unwrap().atlas.insert(rasterized) {
                    Ok(glyph) => {
                        return Some(glyph);
                    },
                    Err(AtlasInsertError::GlyphTooLarge) => {
                        trace!(
                            "Glyph '{}' is too large for grid atlas, will render it using quads",
                            rasterized.rasterized.c
                        );
                        return None;
                    },
                    Err(AtlasInsertError::Full) => {
                        debug!("GridAtlas is full, creating a new one");
                    },
                }
            }

            let index = self.grid_passes.len();
            self.grid_passes.push(GridPass::new(
                index,
                self.columns,
                self.lines,
                self.cell_size,
                self.cell_offset,
            ));
        }
    }

    /// Update cell colors separately from updating glyph. This is needed because glyph itself might
    /// be rendered using quads, but we still need to render background color using main grid pass.
    pub fn update_cell_colors(&mut self, cell: &RenderableCell, wide: bool) {
        let cell_index = cell.line.0 * self.columns + cell.column.0;

        // TODO this should probably be not like this
        // but anyway, cell.bg_alpha has the following semantics in original renderer:
        // 0 == empty cell or regular background color with alpha set to opacity from config
        // 1 == some other background color that is not the default one
        // Non-default bg colors should likely also be transparent, see https://github.com/alacritty/alacritty/pull/4196
        let bg_alpha =
            if cell.bg_alpha == 0.0 { self.bg_alpha } else { (cell.bg_alpha * 255.0) as u8 };
        self.screen_colors_fg[cell_index] = [cell.fg.r, cell.fg.g, cell.fg.b];
        self.screen_colors_bg[cell_index] = [cell.bg.r, cell.bg.g, cell.bg.b, bg_alpha];

        // Wide chars need to update adjacent cell background color too.
        if wide && cell.column.0 < self.columns {
            self.screen_colors_bg[cell_index + 1] = [cell.bg.r, cell.bg.g, cell.bg.b, bg_alpha];
        }
    }

    /// Update cell glyph.
    pub fn update_cell(&mut self, cell: &RenderableCell, glyph: &GridAtlasGlyph) {
        let cell_index = cell.line.0 * self.columns + cell.column.0;

        // put glyph reference into texture data
        self.grid_passes[glyph.atlas_index].glyphs[cell_index] = GlyphRef {
            atlas_x: glyph.column as u8,
            atlas_y: glyph.line as u8,
            flags: GLYPH_REF_FLAG_NOT_EMPTY_BIT
                | if glyph.colored { GLYPH_REF_FLAG_COLORED_BIT } else { 0 },
        };
        self.grid_passes[glyph.atlas_index].dirty = true;
    }

    fn apply_cursor_uniform(&self, pass: usize) {
        match &self.cursor {
            Some(cursor) if cursor.atlas_index == pass => unsafe {
                gl::Uniform4f(
                    self.program.u_cursor,
                    cursor.cell[0],
                    cursor.cell[1],
                    cursor.glyph[0],
                    cursor.glyph[1],
                );
                gl::Uniform3f(
                    self.program.u_cursor_color,
                    cursor.color[0],
                    cursor.color[1],
                    cursor.color[2],
                );
            },
            _ => unsafe {
                gl::Uniform4f(self.program.u_cursor, -1., -1., 0., 0.);
                gl::Uniform3f(self.program.u_cursor_color, 0., 0., 0.);
            },
        }
    }

    /// Render all grid passes
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

        unsafe {
            // Main pass blends glyphs on background manually in shader
            // and it needs to write the final color onto framebuffer as-is
            // so GL blending needs to be disabled
            gl::Disable(gl::BLEND);

            gl::UseProgram(self.program.get_id());

            self.program.set_term_uniforms(size_info);
            gl::Uniform1i(self.program.u_atlas, 0);
            gl::Uniform1i(self.program.u_glyph_ref, 1);
            gl::Uniform1i(self.program.u_color_fg, 2);
            gl::Uniform1i(self.program.u_color_bg, 3);

            gl::ActiveTexture(gl::TEXTURE2);
            gl::BindTexture(gl::TEXTURE_2D, self.screen_colors_fg_tex);
            upload_texture(
                self.columns as i32,
                self.lines as i32,
                PixelFormat::RGB8,
                self.screen_colors_fg.as_ptr() as *const _,
            );

            gl::ActiveTexture(gl::TEXTURE3);
            gl::BindTexture(gl::TEXTURE_2D, self.screen_colors_bg_tex);
            upload_texture(
                self.columns as i32,
                self.lines as i32,
                PixelFormat::RGBA8,
                self.screen_colors_bg.as_ptr() as *const _,
            );

            gl::BindVertexArray(self.vao);
        }

        for (pass_num, pass) in (&self.grid_passes).iter().enumerate() {
            let main_pass = pass_num == 0;
            if !main_pass && !pass.dirty {
                continue;
            }
            let atlas_dims = pass.atlas.cell_dims();
            unsafe {
                gl::Uniform4f(
                    self.program.u_atlas_dim,
                    atlas_dims.offset.x as f32,
                    // Offset needs to be relative to "top" inverted-y OpenGL texture coords
                    (atlas_dims.size.y - atlas_dims.offset.y) as f32 - size_info.cell_height(),
                    atlas_dims.size.x as f32,
                    atlas_dims.size.y as f32,
                );
                gl::Uniform1i(self.program.u_main_pass, main_pass as i32);
                self.apply_cursor_uniform(pass_num);

                gl::ActiveTexture(gl::TEXTURE1);
                gl::BindTexture(gl::TEXTURE_2D, self.screen_glyphs_ref_tex);
                upload_texture(
                    self.columns as i32,
                    self.lines as i32,
                    PixelFormat::RGB8,
                    pass.glyphs.as_ptr() as *const _,
                );

                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, pass.atlas.tex);

                gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
            }

            if main_pass {
                unsafe {
                    // All further passes need to blend with framebuffer color
                    gl::Enable(gl::BLEND);
                    gl::BlendFuncSeparate(gl::ONE, gl::ONE_MINUS_SRC_ALPHA, gl::ONE, gl::ONE);
                }
            }
        }
    }
}

const GLYPH_REF_FLAG_NOT_EMPTY_BIT: u8 = 0b0000_0001;
const GLYPH_REF_FLAG_COLORED_BIT: u8 = 0b0000_0010;

#[derive(Debug, Clone)]
struct GlyphRef {
    // Coordinates into grid atlas
    atlas_x: u8,
    atlas_y: u8,

    // Features of this glyph
    flags: u8,
}

const EMPTY_GLYPH_REF: GlyphRef = GlyphRef { atlas_x: 0, atlas_y: 0, flags: 0 };

#[derive(Debug)]
struct GridPass {
    /// Atlas textures
    atlas: GridAtlas,

    /// Screen worth of glyphs
    glyphs: Vec<GlyphRef>,

    /// Whether this pass contains any data to render
    dirty: bool,
}

impl GridPass {
    fn new(
        index: usize,
        columns: usize,
        lines: usize,
        cell_size: Vec2<i32>,
        cell_offset: Vec2<i32>,
    ) -> Self {
        let cells = columns * lines;
        Self {
            atlas: GridAtlas::new(index, cell_size, cell_offset),
            glyphs: vec![EMPTY_GLYPH_REF; cells],
            dirty: false,
        }
    }

    fn resize(&mut self, columns: usize, lines: usize) {
        let cells = columns * lines;
        self.glyphs.resize(cells, EMPTY_GLYPH_REF);
    }

    fn clear(&mut self) {
        // TODO Can avoid doing this memset if it's not dirty, but have to track whether it's been
        // cleared then
        self.glyphs.iter_mut().for_each(|x| *x = EMPTY_GLYPH_REF);
        self.dirty = false;
    }
}
