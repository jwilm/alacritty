//! This module implements the functionality to render graphic textures
//! in the display.
//!
//! [`RenderList`] is used to track graphics in the visible cells. When all
//! cells in the grid are read, graphics are rendered using the positions
//! found in those cells.

use std::collections::BTreeMap;
use std::mem::{self, MaybeUninit};

use crate::display::content::RenderableCell;
use crate::gl::{self, types::*};
use crate::renderer::graphics::{shader, GraphicsRenderer};

use alacritty_terminal::graphics::GraphicId;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::SizeInfo;

use log::trace;

/// Position to render each texture in the grid.
struct RenderPosition {
    column: Column,
    line: usize,
    offset_x: u16,
    offset_y: u16,
}

/// Track textures to be rendered in the display.
#[derive(Default)]
pub struct RenderList {
    items: BTreeMap<GraphicId, RenderPosition>,
}

impl RenderList {
    /// Detects if the cell contains a graphic, then add it to the render list.
    ///
    /// The graphic is added only the first time it is found in a cell.
    #[inline]
    pub fn update(&mut self, cell: &RenderableCell) {
        if let Some(graphic) = &cell.graphic {
            let graphic_id = graphic.graphic_id();
            if self.items.contains_key(&graphic_id) {
                return;
            }

            let render_item = RenderPosition {
                column: cell.point.column,
                line: cell.point.line,
                offset_x: graphic.offset_x,
                offset_y: graphic.offset_y,
            };

            self.items.insert(graphic_id, render_item);
        }
    }

    /// Returns `true` if there are no items to render.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Builds a list of vertex for the shader program.
    pub fn build_vertices(self, renderer: &GraphicsRenderer) -> Vec<shader::Vertex> {
        use shader::VertexSide::{BottomLeft, BottomRight, TopLeft, TopRight};

        let mut vertices = Vec::new();

        for (graphics_id, render_item) in self.items {
            let graphic_texture = match renderer.graphic_textures.get(&graphics_id) {
                Some(tex) => tex,
                None => continue,
            };

            vertices.reserve(6);

            let vertex = shader::Vertex {
                texture_id: graphic_texture.texture.0,
                sides: TopLeft,
                column: render_item.column.0 as GLuint,
                line: render_item.line as GLuint,
                height: graphic_texture.height,
                width: graphic_texture.width,
                offset_x: render_item.offset_x,
                offset_y: render_item.offset_y,
                base_cell_height: graphic_texture.cell_height,
            };

            vertices.push(vertex);

            for &sides in &[TopRight, BottomLeft, TopRight, BottomRight, BottomLeft] {
                vertices.push(shader::Vertex { sides, ..vertex });
            }
        }

        vertices
    }

    /// Draw graphics in the display, using the graphics rendering shader
    /// program.
    pub fn draw(self, renderer: &GraphicsRenderer, size_info: &SizeInfo) {
        let vertices = self.build_vertices(renderer);

        // Initialize the rendering program.
        unsafe {
            gl::BindBuffer(gl::ARRAY_BUFFER, renderer.program.vbo);
            gl::BindVertexArray(renderer.program.vao);

            gl::UseProgram(renderer.program.id);

            gl::Uniform2f(
                renderer.program.u_cell_dimensions,
                size_info.cell_width(),
                size_info.cell_height(),
            );
            gl::Uniform2f(
                renderer.program.u_view_dimensions,
                size_info.width(),
                size_info.height(),
            );

            gl::BlendFuncSeparate(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA, gl::SRC_ALPHA, gl::ONE);
        }

        // Array for storing the batch to render multiple graphics in a single call to the
        // shader program.
        //
        // Each graphic requires 6 vertices (2 triangles to make a rectangle), and we will
        // never have more than `TEXTURES_ARRAY_SIZE` graphics in a single call, so we set
        // the array size to the maximum value that we can use.
        let mut batch = [MaybeUninit::uninit(); shader::TEXTURES_ARRAY_SIZE * 6];
        let mut batch_size = 0;

        macro_rules! send_batch {
            () => {
                #[allow(unused_assignments)]
                if batch_size > 0 {
                    trace!("Call glDrawArrays with {} items", batch_size);

                    unsafe {
                        gl::BufferData(
                            gl::ARRAY_BUFFER,
                            (batch_size * mem::size_of::<shader::Vertex>()) as isize,
                            batch.as_ptr().cast(),
                            gl::STREAM_DRAW,
                        );

                        gl::DrawArrays(gl::TRIANGLES, 0, batch_size as GLint);
                    }

                    batch_size = 0;
                }
            };
        }

        // In order to send textures to the shader program we need to get a _slot_
        // for every texture associated to a graphic.
        //
        // We have `u_textures.len()` slots available in each execution of the
        // shader.
        //
        // For each slot we need three values:
        //
        // - The texture unit for `glActiveTexture` (`GL_TEXTUREi`).
        // - The uniform location for `textures[i]`.
        // - The index `i`, used to set the value of the uniform.
        //
        // These values are generated using the `tex_slots_generator` iterator.
        //
        // A single graphic has 6 vertices. All vertices will use the same texture
        // slot. To detect if a texture has already a slot, we only need to compare
        // with the last texture (`last_tex_slot`) because all the vertices of a
        // single graphic are consecutive.
        //
        // When all slots are occupied, or the batch array is full, the current
        // batch is sent and the iterator is reset.
        //
        // This logic could be simplified using the [Bindless Texture extension],
        // but it is not a core feature of any OpenGL version, so hardware support
        // is uncertain.
        //
        // [Bindless Texture extension]: https://www.khronos.org/opengl/wiki/Bindless_Texture

        let tex_slots_generator = (gl::TEXTURE0..=gl::TEXTURE31)
            .zip(renderer.program.u_textures.iter())
            .zip(0_u32..)
            .map(|((tex_enum, &u_texture), index)| (tex_enum, u_texture, index));

        let mut tex_slots = tex_slots_generator.clone();

        // Keep the last allocated slot in a `(texture id, index)` tuple.
        let mut last_tex_slot = (0, 0);

        for mut vertex in vertices {
            // Check if we can reuse the last texture slot.
            if last_tex_slot.0 != vertex.texture_id {
                last_tex_slot = loop {
                    match tex_slots.next() {
                        None => {
                            // No more slots. Send the batch and reset the iterator.
                            send_batch!();
                            tex_slots = tex_slots_generator.clone();
                        },

                        Some((tex_enum, u_texture, index)) => {
                            unsafe {
                                gl::ActiveTexture(tex_enum);
                                gl::BindTexture(gl::TEXTURE_2D, vertex.texture_id);
                                gl::Uniform1i(u_texture, index as GLint);
                            }

                            break (vertex.texture_id, index);
                        },
                    }
                };
            }

            vertex.texture_id = last_tex_slot.1;
            batch[batch_size] = MaybeUninit::new(vertex);
            batch_size += 1;

            if batch_size == batch.len() {
                send_batch!();
            }
        }

        send_batch!();

        // Reset state.
        unsafe {
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, 0);

            gl::UseProgram(0);
            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }
    }
}
