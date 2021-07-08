//! This module implements the logic to manage graphic items included in a
//! `Grid` instance.

pub mod sixel;

use std::mem;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::term::color::Rgb;

/// Max allowed dimensions (width, height) for the graphic, in pixels.
pub const MAX_GRAPHIC_DIMENSIONS: [usize; 2] = [4096, 4096];

/// Unique identifier for every graphic added to a grid.
#[derive(Serialize, Deserialize, Eq, PartialEq, Clone, Debug, Copy, Hash, PartialOrd, Ord)]
pub struct GraphicId(u64);

/// Reference to a texture stored in the display.
///
/// When all references to a single texture are removed, its identifier is
/// added to the remove queue.
#[derive(Clone, Debug)]
pub struct TextureRef {
    /// Graphic identifier.
    pub id: GraphicId,

    /// Queue to track removed references.
    pub remove_queue: Weak<Mutex<Vec<GraphicId>>>,
}

impl PartialEq for TextureRef {
    fn eq(&self, t: &Self) -> bool {
        // Ignore remove_queue.
        self.id == t.id
    }
}

impl Eq for TextureRef {}

impl Drop for TextureRef {
    fn drop(&mut self) {
        if let Some(remove_queue) = self.remove_queue.upgrade() {
            remove_queue.lock().push(self.id);
        }
    }
}

/// Graphic data stored in a single cell.
#[derive(Eq, PartialEq, Clone, Debug)]
pub struct GraphicCell {
    /// Texture to draw the graphic in this cell.
    pub texture: Arc<TextureRef>,

    /// Offset in the x direction.
    pub offset_x: u16,

    /// Offset in the y direction.
    pub offset_y: u16,
}

impl GraphicCell {
    /// Graphic identifier of the texture in this cell.
    #[inline]
    pub fn graphic_id(&self) -> GraphicId {
        self.texture.id
    }
}

/// Specifies the format of the pixel data.
#[derive(Serialize, Deserialize, Eq, PartialEq, Clone, Debug, Copy)]
pub enum ColorType {
    /// 3 bytes per pixel (red, green, blue).
    Rgb,

    /// 4 bytes per pixel (red, green, blue, alpha).
    Rgba,
}

/// Defines a single graphic read from the PTY.
#[derive(Serialize, Deserialize, Eq, PartialEq, Clone, Debug)]
pub struct GraphicData {
    /// Graphics identifier.
    pub id: GraphicId,

    /// Width, in pixels, of the graphic.
    pub width: usize,

    /// Height, in pixels, of the graphic.
    pub height: usize,

    /// Color type of the pixels.
    pub color_type: ColorType,

    /// Pixels data.
    pub pixels: Vec<u8>,
}

/// Queues to add or to remove the textures in the display.
pub struct UpdateQueues {
    /// Graphics read from the PTY.
    pub pending: Vec<GraphicData>,

    /// Graphics removed from the grid.
    pub remove_queue: Vec<GraphicId>,
}

/// Track changes in the grid to add or to remove graphics.
#[derive(Clone, Debug, Default)]
pub struct Graphics {
    /// Last generated identifier.
    pub last_id: u64,

    /// New graphics, received from the PTY.
    pub pending: Vec<GraphicData>,

    /// Graphics removed from the grid.
    pub remove_queue: Arc<Mutex<Vec<GraphicId>>>,

    /// Shared palette for Sixel graphics.
    pub sixel_shared_palette: Option<Vec<Rgb>>,
}

impl Graphics {
    /// Generate a new graphic identifier.
    pub fn next_id(&mut self) -> GraphicId {
        self.last_id += 1;
        GraphicId(self.last_id)
    }

    /// Get queues to update graphics in the grid.
    ///
    /// If all queues are empty, it returns `None`.
    pub fn take_queues(&mut self) -> Option<UpdateQueues> {
        let mut remove_queue = self.remove_queue.lock();
        if remove_queue.is_empty() && self.pending.is_empty() {
            return None;
        }

        let remove_queue = mem::take(&mut *remove_queue);

        Some(UpdateQueues { pending: mem::take(&mut self.pending), remove_queue })
    }
}
