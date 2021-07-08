//! This module implements the functionality to support graphics in the grid.

use std::mem;

use alacritty_terminal::graphics::{ColorType, GraphicData, GraphicId, UpdateQueues};
use alacritty_terminal::term::SizeInfo;

use log::trace;
use serde::{Deserialize, Serialize};

use crate::gl;
use crate::gl::types::*;
use crate::renderer;

use std::collections::HashMap;

mod draw;
mod shader;

pub use draw::RenderList;

/// Type for texture names generated in the GPU.
#[derive(Serialize, Deserialize, Eq, PartialEq, Clone, Debug)]
pub struct TextureName(GLuint);

// In debug mode, check if the inner value was set to zero, so we can detect if
// the associated texture was deleted from the GPU.
#[cfg(debug_assertions)]
impl Drop for TextureName {
    fn drop(&mut self) {
        if self.0 != 0 {
            log::error!("Texture {} was not deleted.", self.0);
        }
    }
}

/// Texture for a graphic in the grid.
#[derive(Serialize, Deserialize, PartialEq, Clone, Debug)]
pub struct GraphicTexture {
    /// Texture in the GPU where the graphic pixels are stored.
    texture: TextureName,

    /// Cell height at the moment graphic was created.
    ///
    /// Used to scale it if the user increases or decreases the font size.
    cell_height: f32,

    /// Width in pixels of the graphic.
    width: u16,

    /// Height in pixels of the graphic.
    height: u16,
}

#[derive(Debug)]
pub struct GraphicsRenderer {
    /// Program in the GPU to render graphics.
    program: shader::GraphicsShaderProgram,

    /// Collection to associate graphic identifiers with their textures.
    graphic_textures: HashMap<GraphicId, GraphicTexture>,
}

impl GraphicsRenderer {
    pub fn new() -> Result<GraphicsRenderer, renderer::Error> {
        let program = shader::GraphicsShaderProgram::new()?;
        Ok(GraphicsRenderer { program, graphic_textures: HashMap::default() })
    }

    /// Run the required actions to apply changes for the graphics in the grid.
    #[inline]
    pub fn run_updates(&mut self, update_queues: UpdateQueues, size_info: &SizeInfo) {
        self.remove_graphics(update_queues.remove_queue);
        self.upload_pending_graphics(update_queues.pending, size_info);
    }

    /// Release resources used by removed graphics.
    fn remove_graphics(&mut self, removed_ids: Vec<GraphicId>) {
        let mut textures = Vec::with_capacity(removed_ids.len());
        for id in removed_ids {
            if let Some(mut graphic_texture) = self.graphic_textures.remove(&id) {
                // Reset the inner value of TextureName, so the Drop implementation
                // (in debug mode) can verify that the texture was deleted.
                textures.push(mem::take(&mut graphic_texture.texture.0));
            }
        }

        trace!("Call glDeleteTextures with {} items", textures.len());

        unsafe {
            gl::DeleteTextures(textures.len() as GLint, textures.as_ptr());
        }
    }

    /// Create new textures in the GPU, and upload the pixels to them.
    fn upload_pending_graphics(&mut self, graphics: Vec<GraphicData>, size_info: &SizeInfo) {
        for graphic in graphics {
            let mut texture = 0;

            unsafe {
                gl::GenTextures(1, &mut texture);
                trace!("Texture generated: {}", texture);

                gl::BindTexture(gl::TEXTURE_2D, texture);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAX_LEVEL, 0);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as GLint);

                let pixel_format = match graphic.color_type {
                    ColorType::Rgb => gl::RGB,
                    ColorType::Rgba => gl::RGBA,
                };

                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA as GLint,
                    graphic.width as GLint,
                    graphic.height as GLint,
                    0,
                    pixel_format,
                    gl::UNSIGNED_BYTE,
                    graphic.pixels.as_ptr().cast(),
                );

                gl::BindTexture(gl::TEXTURE_2D, 0);
            }

            let graphic_texture = GraphicTexture {
                texture: TextureName(texture),
                cell_height: size_info.cell_height(),
                width: graphic.width as u16,
                height: graphic.height as u16,
            };

            self.graphic_textures.insert(graphic.id, graphic_texture);
        }
    }

    /// Draw graphics in the display.
    #[inline]
    pub fn draw(&mut self, render_list: RenderList, size_info: &SizeInfo) {
        if !render_list.is_empty() {
            render_list.draw(self, size_info);
        }
    }
}
