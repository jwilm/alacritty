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
//
//! Compatibility layer for different font engines
//!
//! CoreText is used on Mac OS.
//! FreeType is used on everything that's not Mac OS.
//! Eventually, ClearType support will be available for windows

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use, clippy::wrong_pub_self_convention)]

#[cfg(not(any(target_os = "macos", windows)))]
extern crate fontconfig;
#[cfg(not(any(target_os = "macos", windows)))]
extern crate freetype;

#[cfg(target_os = "macos")]
extern crate core_foundation;
#[cfg(target_os = "macos")]
extern crate core_foundation_sys;
#[cfg(target_os = "macos")]
extern crate core_graphics;
#[cfg(target_os = "macos")]
extern crate core_text;
#[cfg(target_os = "macos")]
extern crate euclid;

extern crate libc;

#[cfg(not(any(target_os = "macos", windows)))]
#[macro_use]
extern crate foreign_types;

#[cfg(not(any(target_os = "macos", windows)))]
extern crate harfbuzz_rs;

#[cfg_attr(not(windows), macro_use)]
extern crate log;

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Add, Mul};
use std::sync::atomic::{AtomicUsize, Ordering};

// If target isn't macos or windows, reexport everything from ft
#[cfg(not(any(target_os = "macos", windows)))]
pub mod ft;
#[cfg(not(any(target_os = "macos", windows)))]
pub use ft::{Error, FreeTypeRasterizer as Rasterizer};

#[cfg(windows)]
pub mod directwrite;
#[cfg(windows)]
pub use crate::directwrite::{DirectWriteRasterizer as Rasterizer, Error};

// If target is macos, reexport everything from darwin
#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "macos")]
pub use darwin::*;

/// Placeholder glyph key that represents a blank gylph
pub const PLACEHOLDER_GLYPH: KeyType = KeyType::Placeholder;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontDesc {
    name: String,
    style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slant {
    Normal,
    Italic,
    Oblique,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Normal,
    Bold,
}

/// Style of font
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Style {
    Specific(String),
    Description { slant: Slant, weight: Weight },
}

impl fmt::Display for Style {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Style::Specific(ref s) => f.write_str(&s),
            Style::Description { slant, weight } => {
                write!(f, "slant={:?}, weight={:?}", slant, weight)
            },
        }
    }
}

impl FontDesc {
    pub fn new<S>(name: S, style: Style) -> FontDesc
    where
        S: Into<String>,
    {
        FontDesc { name: name.into(), style }
    }
}

impl fmt::Display for FontDesc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "name {} and style {}", self.name, self.style)
    }
}

/// Identifier for a Font for use in maps/etc
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct FontKey {
    token: u16,
}

impl FontKey {
    /// Get next font key for given size
    ///
    /// The generated key will be globally unique
    pub fn next() -> FontKey {
        static TOKEN: AtomicUsize = AtomicUsize::new(0);

        FontKey { token: TOKEN.fetch_add(1, Ordering::SeqCst) as _ }
    }
}

/// Captures possible outcomes of shaping, if shaping succeeded it will return a `GlyphIndex`.
/// If shaping failed or did not occur, `Fallback` will be returned.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum KeyType {
    /// A valid glyph index from Font face to be rasterized to a glyph
    GlyphIndex(u32),
    /// A character that has not been converted to an index before rasterizing
    Char(char),
    /// Placeholder glyph useful when we need a glyph but it shouldn't ever render as anything
    /// (cursors, wide_char_spacers, etc.)
    Placeholder,
}

impl Default for KeyType {
    fn default() -> Self {
        PLACEHOLDER_GLYPH
    }
}

impl From<u32> for KeyType {
    fn from(val: u32) -> Self {
        KeyType::GlyphIndex(val)
    }
}

impl From<char> for KeyType {
    fn from(val: char) -> Self {
        KeyType::Char(val)
    }
}

#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq)]
pub struct GlyphKey {
    pub id: KeyType,
    pub font_key: FontKey,
    pub size: Size,
}

/// Font size stored as integer
#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Size(i16);

impl Size {
    /// Create a new `Size` from a f32 size in points
    pub fn new(size: f32) -> Size {
        Size((size * Size::factor()) as i16)
    }

    /// Scale factor between font "Size" type and point size
    #[inline]
    pub fn factor() -> f32 {
        2.0
    }

    /// Get the f32 size in points
    pub fn as_f32_pts(self) -> f32 {
        f32::from(self.0) / Size::factor()
    }
}

impl<T: Into<Size>> Add<T> for Size {
    type Output = Size;

    fn add(self, other: T) -> Size {
        Size(self.0.saturating_add(other.into().0))
    }
}

impl<T: Into<Size>> Mul<T> for Size {
    type Output = Size;

    fn mul(self, other: T) -> Size {
        Size(self.0 * other.into().0)
    }
}

impl From<f32> for Size {
    fn from(float: f32) -> Size {
        Size::new(float)
    }
}

#[derive(Clone, Default)]
pub struct RasterizedGlyph {
    pub c: KeyType,
    pub width: i32,
    pub height: i32,
    pub top: i32,
    pub left: i32,
    pub buf: Vec<u8>,
}

struct BufDebugger<'a>(&'a [u8]);

impl<'a> fmt::Debug for BufDebugger<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("GlyphBuffer").field("len", &self.0.len()).field("bytes", &self.0).finish()
    }
}

impl fmt::Debug for RasterizedGlyph {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("RasterizedGlyph")
            .field("c", &self.c)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("top", &self.top)
            .field("left", &self.left)
            .field("buf", &BufDebugger(&self.buf[..]))
            .finish()
    }
}

#[derive(Copy, Clone)]
pub struct Metrics {
    pub average_advance: f64,
    pub line_height: f64,
    pub descent: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
    pub strikeout_position: f32,
    pub strikeout_thickness: f32,
}

pub trait Rasterize {
    /// Errors occurring in Rasterize methods
    type Err: ::std::error::Error + Send + Sync + 'static;

    fn new(
        device_pixel_ratio: f32,
        use_thin_strokes: bool,
        ligatures: bool,
    ) -> Result<Self, Self::Err>
    where
        Self: Sized;

    /// Get `Metrics` for the given `FontKey`
    fn metrics(&self, _: FontKey, _: Size) -> Result<Metrics, Self::Err>;

    /// Load the font described by `FontDesc` and `Size`
    fn load_font(&mut self, _: &FontDesc, _: Size) -> Result<FontKey, Self::Err>;

    /// Rasterize the glyph described by `GlyphKey`.
    fn get_glyph(&mut self, _: GlyphKey) -> Result<RasterizedGlyph, Self::Err>;

    /// Update the Rasterizer's DPI factor
    fn update_dpr(&mut self, device_pixel_ratio: f32);
}

/// Extends the Rasterizer with Harfbuzz specific functionality.
#[cfg(not(any(target_os = "macos", windows)))]
pub trait HbFtExt {
    /// Shape the provided text into a set of glyphs.
    fn shape(&mut self, text: &str, font_key: FontKey) -> harfbuzz_rs::GlyphBuffer;
}
