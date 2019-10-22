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
//! Rasterization powered by FreeType and FontConfig
use std::cmp::min;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use freetype::tt_os2::TrueTypeOS2Table;
use freetype::{self, Library};
use harfbuzz_rs::{Face as HbFace, Feature, Font, GlyphBuffer, Owned, Tag, UnicodeBuffer};
use libc::c_uint;

pub mod fc;

use super::{
    FontDesc, FontKey, GlyphKey, KeyType, Metrics, RasterizedGlyph, Size, Slant, Style, Weight,
};

struct FixedSize {
    pixelsize: f64,
}

struct Face {
    ft_face: freetype::Face,
    key: FontKey,
    load_flags: freetype::face::LoadFlag,
    render_mode: freetype::RenderMode,
    lcd_filter: c_uint,
    non_scalable: Option<FixedSize>,
    hb_font: Owned<Font<'static>>,
    placeholder_glyph_index: u32,
}

impl fmt::Debug for Face {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Face")
            .field("ft_face", &self.ft_face)
            .field("key", &self.key)
            .field("load_flags", &self.load_flags)
            .field("render_mode", &match self.render_mode {
                freetype::RenderMode::Normal => "Normal",
                freetype::RenderMode::Light => "Light",
                freetype::RenderMode::Mono => "Mono",
                freetype::RenderMode::Lcd => "Lcd",
                freetype::RenderMode::LcdV => "LcdV",
                freetype::RenderMode::Max => "Max",
            })
            .field("lcd_filter", &self.lcd_filter)
            .finish()
    }
}

/// Rasterizes glyphs for a single font face.
pub struct FreeTypeRasterizer {
    faces: HashMap<FontKey, Face>,
    library: Library,
    keys: HashMap<PathBuf, FontKey>,
    device_pixel_ratio: f32,
    use_font_ligatures: bool,
}

#[inline]
fn to_freetype_26_6(f: f32) -> isize {
    ((1i32 << 6) as f32 * f) as isize
}

impl ::Rasterize for FreeTypeRasterizer {
    type Err = Error;

    fn new(device_pixel_ratio: f32, _: bool, ligatures: bool) -> Result<FreeTypeRasterizer, Error> {
        let library = Library::init()?;

        Ok(FreeTypeRasterizer {
            faces: HashMap::new(),
            keys: HashMap::new(),
            library,
            device_pixel_ratio,
            use_font_ligatures: ligatures,
        })
    }

    fn metrics(&self, key: FontKey, _size: Size) -> Result<Metrics, Error> {
        let face = self.faces.get(&key).ok_or(Error::FontNotLoaded)?;
        let full = self.full_metrics(key)?;

        let height = (full.size_metrics.height / 64) as f64;
        let descent = (full.size_metrics.descender / 64) as f32;

        // Get underline position and thickness in device pixels
        let x_scale = full.size_metrics.x_scale as f32 / 65536.0;
        let mut underline_position = f32::from(face.ft_face.underline_position()) * x_scale / 64.;
        let mut underline_thickness = f32::from(face.ft_face.underline_thickness()) * x_scale / 64.;

        // Fallback for bitmap fonts which do not provide underline metrics
        if underline_position == 0. {
            underline_thickness = (descent / 5.).round();
            underline_position = descent / 2.;
        }

        // Get strikeout position and thickness in device pixels
        let (strikeout_position, strikeout_thickness) =
            match TrueTypeOS2Table::from_face(&mut face.ft_face.clone()) {
                Some(os2) => {
                    let strikeout_position = f32::from(os2.y_strikeout_position()) * x_scale / 64.;
                    let strikeout_thickness = f32::from(os2.y_strikeout_size()) * x_scale / 64.;
                    (strikeout_position, strikeout_thickness)
                },
                _ => {
                    // Fallback if font doesn't provide info about strikeout
                    trace!("Using fallback strikeout metrics");
                    let strikeout_position = height as f32 / 2. + descent;
                    (strikeout_position, underline_thickness)
                },
            };

        Ok(Metrics {
            average_advance: full.cell_width,
            line_height: height,
            descent,
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness,
        })
    }

    fn load_font(&mut self, desc: &FontDesc, size: Size) -> Result<FontKey, Error> {
        self.get_face(desc, size)
    }

    fn get_glyph(&mut self, glyph_key: GlyphKey) -> Result<RasterizedGlyph, Error> {
        self.get_rendered_glyph(glyph_key)
    }

    fn update_dpr(&mut self, device_pixel_ratio: f32) {
        self.device_pixel_ratio = device_pixel_ratio;
    }
}

impl crate::HbFtExt for FreeTypeRasterizer {
    fn shape(&mut self, text: &str, font_key: FontKey) -> GlyphBuffer {
        let hb_font = &self.faces[&font_key].hb_font;
        let buf = UnicodeBuffer::new().add_str(text);
        let use_font_ligatures = if self.use_font_ligatures { 1 } else { 0 };
        let features = [
            Feature::new(Tag::new('l', 'i', 'g', 'a'), use_font_ligatures, ..),
            Feature::new(Tag::new('c', 'a', 'l', 't'), use_font_ligatures, ..),
        ];
        harfbuzz_rs::shape(hb_font, buf, &features)
    }
}

pub trait IntoFontconfigType {
    type FcType;
    fn into_fontconfig_type(&self) -> Self::FcType;
}

impl IntoFontconfigType for Slant {
    type FcType = fc::Slant;

    fn into_fontconfig_type(&self) -> Self::FcType {
        match *self {
            Slant::Normal => fc::Slant::Roman,
            Slant::Italic => fc::Slant::Italic,
            Slant::Oblique => fc::Slant::Oblique,
        }
    }
}

impl IntoFontconfigType for Weight {
    type FcType = fc::Weight;

    fn into_fontconfig_type(&self) -> Self::FcType {
        match *self {
            Weight::Normal => fc::Weight::Regular,
            Weight::Bold => fc::Weight::Bold,
        }
    }
}

struct FullMetrics {
    size_metrics: freetype::ffi::FT_Size_Metrics,
    cell_width: f64,
}

impl FreeTypeRasterizer {
    /// Load a font face according to `FontDesc`
    fn get_face(&mut self, desc: &FontDesc, size: Size) -> Result<FontKey, Error> {
        // Adjust for DPI
        let size = Size::new(size.as_f32_pts() * self.device_pixel_ratio * 96. / 72.);

        match desc.style {
            Style::Description { slant, weight } => {
                // Match nearest font
                self.get_matching_face(&desc, slant, weight, size)
            },
            Style::Specific(ref style) => {
                // If a name was specified, try and load specifically that font.
                self.get_specific_face(&desc, &style, size)
            },
        }
    }

    fn full_metrics(&self, key: FontKey) -> Result<FullMetrics, Error> {
        let face = self.faces.get(&key).ok_or(Error::FontNotLoaded)?;

        let size_metrics = face.ft_face.size_metrics().ok_or(Error::MissingSizeMetrics)?;

        let width = match face.ft_face.load_char('0' as usize, face.load_flags) {
            Ok(_) => face.ft_face.glyph().metrics().horiAdvance / 64,
            Err(_) => size_metrics.max_advance / 64,
        } as f64;

        Ok(FullMetrics { size_metrics, cell_width: width })
    }

    fn get_matching_face(
        &mut self,
        desc: &FontDesc,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<FontKey, Error> {
        let mut pattern = fc::Pattern::new();
        pattern.add_family(&desc.name);
        pattern.set_weight(weight.into_fontconfig_type());
        pattern.set_slant(slant.into_fontconfig_type());
        pattern.add_pixelsize(f64::from(size.as_f32_pts()));

        let font = fc::font_match(fc::Config::get_current(), &mut pattern)
            .ok_or_else(|| Error::MissingFont(desc.to_owned()))?;

        self.face_from_pattern(&font).and_then(|pattern| {
            pattern.map(Ok).unwrap_or_else(|| Err(Error::MissingFont(desc.to_owned())))
        })
    }

    fn get_specific_face(
        &mut self,
        desc: &FontDesc,
        style: &str,
        size: Size,
    ) -> Result<FontKey, Error> {
        let mut pattern = fc::Pattern::new();
        pattern.add_family(&desc.name);
        pattern.add_style(style);
        pattern.add_pixelsize(f64::from(size.as_f32_pts()));

        let font = fc::font_match(fc::Config::get_current(), &mut pattern)
            .ok_or_else(|| Error::MissingFont(desc.to_owned()))?;
        self.face_from_pattern(&font).and_then(|pattern| {
            pattern.map(Ok).unwrap_or_else(|| Err(Error::MissingFont(desc.to_owned())))
        })
    }

    fn face_from_pattern(&mut self, pattern: &fc::Pattern) -> Result<Option<FontKey>, Error> {
        if let (Some(path), Some(index)) = (pattern.file(0), pattern.index().nth(0)) {
            if let Some(key) = self.keys.get(&path) {
                return Ok(Some(*key));
            }

            trace!("Got font path={:?}", path);
            let mut ft_face = self.library.new_face(&path, index)?;

            // This will be different for each font so we can't use a constant but we don't want to
            // look it up every time so we cache it on font load.
            let placeholder_glyph_index = ft_face.get_char_index(' ' as usize);

            // Get available pixel sizes if font isn't scalable.
            let non_scalable = if pattern.scalable().next().unwrap_or(true) {
                None
            } else {
                let mut pixelsize = pattern.pixelsize();
                debug!("pixelsizes: {:?}", pixelsize);

                Some(FixedSize { pixelsize: pixelsize.next().expect("has 1+ pixelsize") })
            };

            // Construct harfbuzz font
            let hb_font = Font::new(HbFace::from_file(&path, index as u32)?);
            //let hb_font = unsafe {
            //    // Cast here is done to convert a C struct from the freetype-rs Rust type to the
            //    // freetype Rust type that harfbuzz accepts.
            //    let hb_font_raw = harfbuzz_rs::hb::hb_ft_font_create_referenced(ft_face.raw_mut()
            //        as *mut _
            //        as *mut u64
            //        as *mut _);
            //    harfbuzz_rs::Owned::from_raw(hb_font_raw)
            //};

            let face = Face {
                ft_face,
                key: FontKey::next(),
                load_flags: Self::ft_load_flags(pattern),
                render_mode: Self::ft_render_mode(pattern),
                lcd_filter: Self::ft_lcd_filter(pattern),
                non_scalable,
                hb_font,
                placeholder_glyph_index,
            };

            debug!("Loaded Face {:?}", face);

            let key = face.key;
            self.faces.insert(key, face);
            self.keys.insert(path, key);

            Ok(Some(key))
        } else {
            Ok(None)
        }
    }

    fn face_for_glyph(&mut self, glyph_key: GlyphKey) -> FontKey {
        match glyph_key.id {
            // We already found a glyph index, use current font
            KeyType::GlyphIndex(_) | KeyType::Placeholder => glyph_key.font_key,
            // Harfbuzz failed to find a glyph index, try to load a font for c
            KeyType::Char(c) => self.load_face_with_glyph(c).unwrap_or(glyph_key.font_key),
        }
    }

    fn get_rendered_glyph(&mut self, glyph_key: GlyphKey) -> Result<RasterizedGlyph, Error> {
        // Render a normal character if it's not a cursor
        let font_key = self.face_for_glyph(glyph_key);
        let face = &self.faces[&font_key];
        let index = match glyph_key.id {
            KeyType::GlyphIndex(i) => i,
            KeyType::Char(c) => face.ft_face.get_char_index(c as usize),
            KeyType::Placeholder => face.placeholder_glyph_index,
        };

        let size =
            face.non_scalable.as_ref().map(|v| v.pixelsize as f32).unwrap_or_else(|| {
                glyph_key.size.as_f32_pts() * self.device_pixel_ratio * 96. / 72.
            });

        face.ft_face.set_char_size(to_freetype_26_6(size), 0, 0, 0)?;

        unsafe {
            let ft_lib = self.library.raw();
            freetype::ffi::FT_Library_SetLcdFilter(ft_lib, face.lcd_filter);
        }

        face.ft_face.load_glyph(index as u32, face.load_flags)?;
        let glyph = face.ft_face.glyph();
        glyph.render_glyph(face.render_mode)?;

        let (pixel_height, pixel_width, buf) = Self::normalize_buffer(&glyph.bitmap())?;

        Ok(RasterizedGlyph {
            c: glyph_key.id,
            top: glyph.bitmap_top(),
            left: glyph.bitmap_left(),
            width: pixel_width,
            height: pixel_height,
            buf,
        })
    }

    fn ft_load_flags(pat: &fc::Pattern) -> freetype::face::LoadFlag {
        let antialias = pat.antialias().next().unwrap_or(true);
        let hinting = pat.hintstyle().next().unwrap_or(fc::HintStyle::Slight);
        let rgba = pat.rgba().next().unwrap_or(fc::Rgba::Unknown);
        let embedded_bitmaps = pat.embeddedbitmap().next().unwrap_or(true);

        use freetype::face::LoadFlag;
        let mut flags = match (antialias, hinting, rgba) {
            (false, fc::HintStyle::None, _) => LoadFlag::NO_HINTING | LoadFlag::MONOCHROME,
            (false, ..) => LoadFlag::TARGET_MONO | LoadFlag::MONOCHROME,
            (true, fc::HintStyle::None, _) => LoadFlag::NO_HINTING | LoadFlag::TARGET_NORMAL,
            // hintslight does *not* use LCD hinting even when a subpixel mode
            // is selected.
            //
            // According to the FreeType docs,
            //
            // > You can use a hinting algorithm that doesn't correspond to the
            // > same rendering mode.  As an example, it is possible to use the
            // > ‘light’ hinting algorithm and have the results rendered in
            // > horizontal LCD pixel mode.
            //
            // In practice, this means we can have `FT_LOAD_TARGET_LIGHT` with
            // subpixel render modes like `FT_RENDER_MODE_LCD`. Libraries like
            // cairo take the same approach and consider `hintslight` to always
            // prefer `FT_LOAD_TARGET_LIGHT`
            (true, fc::HintStyle::Slight, _) => LoadFlag::TARGET_LIGHT,
            // If LCD hinting is to be used, must select hintmedium or hintfull,
            // have AA enabled, and select a subpixel mode.
            (true, _, fc::Rgba::Rgb) | (true, _, fc::Rgba::Bgr) => LoadFlag::TARGET_LCD,
            (true, _, fc::Rgba::Vrgb) | (true, _, fc::Rgba::Vbgr) => LoadFlag::TARGET_LCD_V,
            // For non-rgba modes with either Medium or Full hinting, just use
            // the default hinting algorithm.
            //
            // TODO should Medium/Full control whether to use the auto hinter?
            (true, _, fc::Rgba::Unknown) => LoadFlag::TARGET_NORMAL,
            (true, _, fc::Rgba::None) => LoadFlag::TARGET_NORMAL,
        };

        if !embedded_bitmaps {
            flags |= LoadFlag::NO_BITMAP;
        }

        flags
    }

    fn ft_render_mode(pat: &fc::Pattern) -> freetype::RenderMode {
        let antialias = pat.antialias().next().unwrap_or(true);
        let rgba = pat.rgba().next().unwrap_or(fc::Rgba::Unknown);

        match (antialias, rgba) {
            (false, _) => freetype::RenderMode::Mono,
            (_, fc::Rgba::Rgb) | (_, fc::Rgba::Bgr) => freetype::RenderMode::Lcd,
            (_, fc::Rgba::Vrgb) | (_, fc::Rgba::Vbgr) => freetype::RenderMode::LcdV,
            (true, _) => freetype::RenderMode::Normal,
        }
    }

    fn ft_lcd_filter(pat: &fc::Pattern) -> c_uint {
        match pat.lcdfilter().next().unwrap_or(fc::LcdFilter::Default) {
            fc::LcdFilter::None => freetype::ffi::FT_LCD_FILTER_NONE,
            fc::LcdFilter::Default => freetype::ffi::FT_LCD_FILTER_DEFAULT,
            fc::LcdFilter::Light => freetype::ffi::FT_LCD_FILTER_LIGHT,
            fc::LcdFilter::Legacy => freetype::ffi::FT_LCD_FILTER_LEGACY,
        }
    }

    /// Given a FreeType `Bitmap`, returns packed buffer with 1 byte per LCD channel.
    ///
    /// The i32 value in the return type is the number of pixels per row.
    fn normalize_buffer(
        bitmap: &freetype::bitmap::Bitmap,
    ) -> freetype::FtResult<(i32, i32, Vec<u8>)> {
        use freetype::bitmap::PixelMode;

        let buf = bitmap.buffer();
        let mut packed = Vec::with_capacity((bitmap.rows() * bitmap.width()) as usize);
        let pitch = bitmap.pitch().abs() as usize;
        match bitmap.pixel_mode()? {
            PixelMode::Lcd => {
                for i in 0..bitmap.rows() {
                    let start = (i as usize) * pitch;
                    let stop = start + bitmap.width() as usize;
                    packed.extend_from_slice(&buf[start..stop]);
                }
                Ok((bitmap.rows(), bitmap.width() / 3, packed))
            },
            PixelMode::LcdV => {
                for i in 0..bitmap.rows() / 3 {
                    for j in 0..bitmap.width() {
                        for k in 0..3 {
                            let offset = ((i as usize) * 3 + k) * pitch + (j as usize);
                            packed.push(buf[offset]);
                        }
                    }
                }
                Ok((bitmap.rows() / 3, bitmap.width(), packed))
            },
            // Mono data is stored in a packed format using 1 bit per pixel.
            PixelMode::Mono => {
                fn unpack_byte(res: &mut Vec<u8>, byte: u8, mut count: u8) {
                    // Mono stores MSBit at top of byte
                    let mut bit = 7;
                    while count != 0 {
                        let value = ((byte >> bit) & 1) * 255;
                        // Push value 3x since result buffer should be 1 byte
                        // per channel
                        res.push(value);
                        res.push(value);
                        res.push(value);
                        count -= 1;
                        bit -= 1;
                    }
                };

                for i in 0..(bitmap.rows() as usize) {
                    let mut columns = bitmap.width();
                    let mut byte = 0;
                    let offset = i * bitmap.pitch().abs() as usize;
                    while columns != 0 {
                        let bits = min(8, columns);
                        unpack_byte(&mut packed, buf[offset + byte], bits as u8);

                        columns -= bits;
                        byte += 1;
                    }
                }
                Ok((bitmap.rows(), bitmap.width(), packed))
            },
            // Gray data is stored as a value between 0 and 255 using 1 byte per pixel.
            PixelMode::Gray => {
                for i in 0..bitmap.rows() {
                    let start = (i as usize) * pitch;
                    let stop = start + bitmap.width() as usize;
                    for byte in &buf[start..stop] {
                        packed.push(*byte);
                        packed.push(*byte);
                        packed.push(*byte);
                    }
                }
                Ok((bitmap.rows(), bitmap.width(), packed))
            },
            mode => panic!("unhandled pixel mode: {:?}", mode),
        }
    }

    fn load_face_with_glyph(&mut self, glyph: char) -> Result<FontKey, Error> {
        let mut charset = fc::CharSet::new();
        charset.add(glyph);
        let mut pattern = fc::Pattern::new();
        pattern.add_charset(&charset);

        let config = fc::Config::get_current();
        match fc::font_match(config, &mut pattern) {
            Some(pattern) => {
                if let (Some(path), Some(_)) = (pattern.file(0), pattern.index().nth(0)) {
                    match self.keys.get(&path) {
                        // We've previously loaded this font, so don't
                        // load it again.
                        Some(&key) => {
                            debug!("Hit for font {:?}; no need to load", path);
                            Ok(key)
                        },

                        None => {
                            debug!("Miss for font {:?}; loading now", path);
                            // Safe to unwrap the option since we've already checked for the path
                            // and index above.
                            let key = self.face_from_pattern(&pattern)?.unwrap();
                            Ok(key)
                        },
                    }
                } else {
                    Err(Error::MissingFont(FontDesc::new(
                        "fallback-without-path",
                        Style::Specific(glyph.to_string()),
                    )))
                }
            },
            None => Err(Error::MissingFont(FontDesc::new(
                "no-fallback-for",
                Style::Specific(glyph.to_string()),
            ))),
        }
    }
}

/// Errors occurring when using the freetype rasterizer
#[derive(Debug)]
pub enum Error {
    /// Error occurred within the FreeType library
    FreeType(freetype::Error),

    /// Couldn't find font matching description
    MissingFont(FontDesc),

    /// Tried to get size metrics from a Face that didn't have a size
    MissingSizeMetrics,

    /// Requested an operation with a FontKey that isn't known to the rasterizer
    FontNotLoaded,

    /// Error occurred during IO operation (loading harfbuzz font face, etc.)
    IO(std::io::Error),
}

impl ::std::error::Error for Error {
    fn cause(&self) -> Option<&dyn std::error::Error> {
        match *self {
            Error::FreeType(ref err) => Some(err),
            Error::IO(ref err) => Some(err),
            _ => None,
        }
    }

    fn description(&self) -> &str {
        match *self {
            Error::FreeType(ref err) => err.description(),
            Error::IO(ref err) => err.description(),
            Error::MissingFont(ref _desc) => "Couldn't find the requested font",
            Error::FontNotLoaded => "Tried to operate on font that hasn't been loaded",
            Error::MissingSizeMetrics => "Tried to get size metrics from a face without a size",
        }
    }
}

impl ::std::fmt::Display for Error {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        match *self {
            Error::FreeType(ref err) => err.fmt(f),
            Error::IO(ref err) => err.fmt(f),
            Error::MissingFont(ref desc) => write!(
                f,
                "Couldn't find a font with {}\n\tPlease check the font config in your \
                 alacritty.yml.",
                desc
            ),
            Error::FontNotLoaded => f.write_str("Tried to use a font that hasn't been loaded"),
            Error::MissingSizeMetrics => {
                f.write_str("Tried to get size metrics from a face without a size")
            },
        }
    }
}

impl From<freetype::Error> for Error {
    fn from(val: freetype::Error) -> Error {
        Error::FreeType(val)
    }
}
impl From<std::io::Error> for Error {
    fn from(val: std::io::Error) -> Error {
        Error::IO(val)
    }
}

unsafe impl Send for FreeTypeRasterizer {}
