// Copyright 2016 Joe Wilm, The Alacritty Project Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Helpers for creating different cursor glyphs from font metrics

use std::cmp;

use serde::Deserialize;

use font::{Metrics, RasterizedGlyph, PLACEHOLDER_GLYPH};

use crate::ansi::CursorStyle;

/// Width/Height of the cursor relative to the font width
pub const CURSOR_WIDTH_PERCENTAGE: i32 = 15;

/// A key for caching cursor glyphs
#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash, Deserialize)]
pub struct CursorKey {
    pub style: CursorStyle,
    pub is_wide: bool,
}

pub fn get_cursor_glyph(
    cursor: CursorStyle,
    metrics: Metrics,
    offset_x: i8,
    offset_y: i8,
    is_wide: bool,
) -> RasterizedGlyph {
    // Calculate the cell metrics
    let height = metrics.line_height as i32 + i32::from(offset_y);
    let mut width = metrics.average_advance as i32 + i32::from(offset_x);
    let line_width = cmp::max(width * CURSOR_WIDTH_PERCENTAGE / 100, 1);

    // Double the cursor width if it's above a double-width glyph
    if is_wide {
        width *= 2;
    }

    match cursor {
        CursorStyle::HollowBlock => get_box_cursor_glyph(height, width, line_width),
        CursorStyle::Underline => get_underline_cursor_glyph(width, line_width),
        CursorStyle::Beam => get_beam_cursor_glyph(height, line_width),
        CursorStyle::Block => get_block_cursor_glyph(height, width),
        CursorStyle::Hidden => RasterizedGlyph::default(),
    }
}

// Returns a custom underline cursor character
pub fn get_underline_cursor_glyph(width: i32, line_width: i32) -> RasterizedGlyph {
    // Create a new rectangle, the height is relative to the font width
    let buf = vec![255u8; (width * line_width * 3) as usize];

    // Create a custom glyph with the rectangle data attached to it
    RasterizedGlyph {
        c: PLACEHOLDER_GLYPH,
        top: line_width,
        left: 0,
        height: line_width,
        width,
        buf,
    }
}

// Returns a custom beam cursor character
pub fn get_beam_cursor_glyph(height: i32, line_width: i32) -> RasterizedGlyph {
    // Create a new rectangle that is at least one pixel wide
    let buf = vec![255u8; (line_width * height * 3) as usize];

    // Create a custom glyph with the rectangle data attached to it
    RasterizedGlyph { c: PLACEHOLDER_GLYPH, top: height, left: 0, height, width: line_width, buf }
}

// Returns a custom box cursor character
pub fn get_box_cursor_glyph(height: i32, width: i32, line_width: i32) -> RasterizedGlyph {
    // Create a new box outline rectangle
    let mut buf = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            if y < line_width
                || y >= height - line_width
                || x < line_width
                || x >= width - line_width
            {
                buf.append(&mut vec![255u8; 3]);
            } else {
                buf.append(&mut vec![0u8; 3]);
            }
        }
    }

    // Create a custom glyph with the rectangle data attached to it
    RasterizedGlyph { c: PLACEHOLDER_GLYPH, top: height, left: 0, height, width, buf }
}

// Returns a custom block cursor character
pub fn get_block_cursor_glyph(height: i32, width: i32) -> RasterizedGlyph {
    // Create a completely filled glyph
    let buf = vec![255u8; (width * height * 3) as usize];

    // Create a custom glyph with the rectangle data attached to it
    RasterizedGlyph { c: PLACEHOLDER_GLYPH, top: height, left: 0, height, width, buf }
}
