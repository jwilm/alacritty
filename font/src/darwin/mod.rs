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
//! Font rendering based on CoreText
#![allow(improper_ctypes)]
use std::collections::HashMap;
use std::path::PathBuf;
use std::ptr;

use {Slant, Style, Weight};

use core_foundation::array::{CFArray, CFIndex};
use core_foundation::string::CFString;
use core_graphics::base::kCGImageAlphaPremultipliedFirst;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::font::{CGFont, CGGlyph};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_text::font::{
    cascade_list_for_languages as ct_cascade_list_for_languages,
    new_from_descriptor as ct_new_from_descriptor, CTFont,
};
use core_text::font_collection::create_for_family;
use core_text::font_collection::get_family_names as ct_get_family_names;
use core_text::font_descriptor::kCTFontDefaultOrientation;
use core_text::font_descriptor::kCTFontHorizontalOrientation;
use core_text::font_descriptor::kCTFontVerticalOrientation;
use core_text::font_descriptor::SymbolicTraitAccessors;
use core_text::font_descriptor::{CTFontDescriptor, CTFontOrientation};

use euclid::{Point2D, Rect, Size2D};

use super::{FontDesc, FontKey, GlyphKey, KeyType, Metrics, RasterizedGlyph};

pub mod byte_order;
use self::byte_order::extract_rgb;
use self::byte_order::kCGBitmapByteOrder32Host;

use super::Size;

/// Font descriptor
///
/// The descriptor provides data about a font and supports creating a font.
#[derive(Debug)]
pub struct Descriptor {
    family_name: String,
    font_name: String,
    style_name: String,
    display_name: String,
    font_path: PathBuf,

    ct_descriptor: CTFontDescriptor,
}

impl Descriptor {
    fn new(desc: CTFontDescriptor) -> Descriptor {
        Descriptor {
            family_name: desc.family_name(),
            font_name: desc.font_name(),
            style_name: desc.style_name(),
            display_name: desc.display_name(),
            font_path: desc.font_path().unwrap_or_else(PathBuf::new),
            ct_descriptor: desc,
        }
    }
}

/// Rasterizer, the main type exported by this package
///
/// Given a fontdesc, can rasterize fonts.
pub struct Rasterizer {
    fonts: HashMap<FontKey, Font>,
    keys: HashMap<(FontDesc, Size), FontKey>,
    device_pixel_ratio: f32,
    use_thin_strokes: bool,
}

/// Errors occurring when using the core text rasterizer
#[derive(Debug)]
pub enum Error {
    /// Tried to rasterize a glyph but it was not available
    MissingGlyph(KeyType),

    /// Couldn't find font matching description
    MissingFont(FontDesc),

    /// Requested an operation with a FontKey that isn't known to the rasterizer
    FontNotLoaded,
}

impl ::std::error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::MissingGlyph(ref _c) => "Couldn't find the requested glyph",
            Error::MissingFont(ref _desc) => "Couldn't find the requested font",
            Error::FontNotLoaded => "Tried to operate on font that hasn't been loaded",
        }
    }
}

impl ::std::fmt::Display for Error {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        match *self {
            Error::MissingGlyph(ref c) => write!(f, "Glyph not found for char {:?}", c),
            Error::MissingFont(ref desc) => write!(
                f,
                "Couldn't find a font with {}\n\tPlease check the font config in your \
                 alacritty.yml.",
                desc
            ),
            Error::FontNotLoaded => f.write_str("Tried to use a font that hasn't been loaded"),
        }
    }
}

impl ::Rasterize for Rasterizer {
    type Err = Error;

    fn new(device_pixel_ratio: f32, use_thin_strokes: bool, _: bool) -> Result<Rasterizer, Error> {
        Ok(Rasterizer {
            fonts: HashMap::new(),
            keys: HashMap::new(),
            device_pixel_ratio,
            use_thin_strokes,
        })
    }

    /// Get metrics for font specified by FontKey
    fn metrics(&self, key: FontKey, _size: Size) -> Result<Metrics, Error> {
        let font = self.fonts.get(&key).ok_or(Error::FontNotLoaded)?;

        Ok(font.metrics())
    }

    fn load_font(&mut self, desc: &FontDesc, size: Size) -> Result<FontKey, Error> {
        let scaled_size = Size::new(size.as_f32_pts() * self.device_pixel_ratio);
        self.keys.get(&(desc.to_owned(), scaled_size)).map(|k| Ok(*k)).unwrap_or_else(|| {
            let font = self.get_font(desc, size)?;
            let key = FontKey::next();

            self.fonts.insert(key, font);
            self.keys.insert((desc.clone(), scaled_size), key);

            Ok(key)
        })
    }

    /// Get rasterized glyph for given glyph key
    fn get_glyph(&mut self, glyph: GlyphKey) -> Result<RasterizedGlyph, Error> {
        // get loaded font
        let font = self.fonts.get(&glyph.font_key).ok_or(Error::FontNotLoaded)?;

        // first try the font itself as a direct hit
        self.maybe_get_glyph(glyph, font).unwrap_or_else(|| {
            // then try fallbacks
            for fallback in &font.fallbacks {
                if let Some(result) = self.maybe_get_glyph(glyph, &fallback) {
                    // found a fallback
                    return result;
                }
            }
            // no fallback, give up.
            Err(Error::MissingGlyph(glyph.id))
        })
    }

    fn update_dpr(&mut self, device_pixel_ratio: f32) {
        self.device_pixel_ratio = device_pixel_ratio;
    }
}

impl Rasterizer {
    fn get_specific_face(
        &mut self,
        desc: &FontDesc,
        style: &str,
        size: Size,
    ) -> Result<Font, Error> {
        let descriptors = descriptors_for_family(&desc.name[..]);
        for descriptor in descriptors {
            if descriptor.style_name == style {
                // Found the font we want
                let scaled_size = f64::from(size.as_f32_pts()) * f64::from(self.device_pixel_ratio);
                let font = descriptor.to_font(scaled_size, true);
                return Ok(font);
            }
        }

        Err(Error::MissingFont(desc.to_owned()))
    }

    fn get_matching_face(
        &mut self,
        desc: &FontDesc,
        slant: Slant,
        weight: Weight,
        size: Size,
    ) -> Result<Font, Error> {
        let bold = match weight {
            Weight::Bold => true,
            _ => false,
        };
        let italic = match slant {
            Slant::Normal => false,
            _ => true,
        };
        let scaled_size = f64::from(size.as_f32_pts()) * f64::from(self.device_pixel_ratio);

        let descriptors = descriptors_for_family(&desc.name[..]);
        for descriptor in descriptors {
            let font = descriptor.to_font(scaled_size, true);
            if font.is_bold() == bold && font.is_italic() == italic {
                // Found the font we want
                return Ok(font);
            }
        }

        Err(Error::MissingFont(desc.to_owned()))
    }

    fn get_font(&mut self, desc: &FontDesc, size: Size) -> Result<Font, Error> {
        match desc.style {
            Style::Specific(ref style) => self.get_specific_face(desc, style, size),
            Style::Description { slant, weight } => {
                self.get_matching_face(desc, slant, weight, size)
            },
        }
    }

    // Helper to try and get a glyph for a given font. Used for font fallback.
    fn maybe_get_glyph(
        &self,
        glyph: GlyphKey,
        font: &Font,
    ) -> Option<Result<RasterizedGlyph, Error>> {
        let scaled_size = self.device_pixel_ratio * glyph.size.as_f32_pts();
        font.get_glyph(glyph.id, f64::from(scaled_size), self.use_thin_strokes)
            .map(|r| Some(Ok(r)))
            .unwrap_or_else(|e| match e {
                Error::MissingGlyph(_) => None,
                _ => Some(Err(e)),
            })
    }
}

/// Specifies the intended rendering orientation of the font for obtaining glyph metrics
#[derive(Debug)]
pub enum FontOrientation {
    Default = kCTFontDefaultOrientation as isize,
    Horizontal = kCTFontHorizontalOrientation as isize,
    Vertical = kCTFontVerticalOrientation as isize,
}

impl Default for FontOrientation {
    fn default() -> FontOrientation {
        FontOrientation::Default
    }
}

/// A font
#[derive(Clone)]
pub struct Font {
    ct_font: CTFont,
    cg_font: CGFont,
    fallbacks: Vec<Font>,
}

unsafe impl Send for Font {}

/// List all family names
pub fn get_family_names() -> Vec<String> {
    // CFArray of CFStringRef
    let names = ct_get_family_names();
    let mut owned_names = Vec::new();

    for name in names.iter() {
        owned_names.push(name.to_string());
    }

    owned_names
}

/// Return fallback descriptors for font/language list
fn cascade_list_for_languages(ct_font: &CTFont, languages: &[String]) -> Vec<Descriptor> {
    // convert language type &Vec<String> -> CFArray
    let langarr: CFArray<CFString> = {
        let tmp: Vec<CFString> =
            languages.iter().map(|language| CFString::new(&language)).collect();
        CFArray::from_CFTypes(&tmp)
    };

    // CFArray of CTFontDescriptorRef (again)
    let list = ct_cascade_list_for_languages(ct_font, &langarr);

    // convert CFArray to Vec<Descriptor>
    list.into_iter().map(|fontdesc| Descriptor::new(fontdesc.clone())).collect()
}

/// Get descriptors for family name
pub fn descriptors_for_family(family: &str) -> Vec<Descriptor> {
    let mut out = Vec::new();

    trace!("Family: {}", family);
    let ct_collection = create_for_family(family).unwrap_or_else(|| {
        // Fallback to Menlo if we can't find the config specified font family.
        warn!("Unable to load specified font {}, falling back to Menlo", &family);
        create_for_family("Menlo").expect("Menlo exists")
    });

    // CFArray of CTFontDescriptorRef (i think)
    let descriptors = ct_collection.get_descriptors();
    if let Some(descriptors) = descriptors {
        for descriptor in descriptors.iter() {
            out.push(Descriptor::new(descriptor.clone()));
        }
    }

    out
}

impl Descriptor {
    /// Create a Font from this descriptor
    pub fn to_font(&self, size: f64, load_fallbacks: bool) -> Font {
        let ct_font = ct_new_from_descriptor(&self.ct_descriptor, size);
        let cg_font = ct_font.copy_to_CGFont();

        let fallbacks = if load_fallbacks {
            descriptors_for_family("Menlo")
                .into_iter()
                .filter(|d| d.font_name == "Menlo-Regular")
                .nth(0)
                .map(|descriptor| {
                    let menlo = ct_new_from_descriptor(&descriptor.ct_descriptor, size);

                    // TODO fixme, hardcoded en for english
                    let mut fallbacks = cascade_list_for_languages(&menlo, &["en".to_owned()])
                        .into_iter()
                        .filter(|desc| !desc.font_path.as_os_str().is_empty())
                        .map(|desc| desc.to_font(size, false))
                        .collect::<Vec<_>>();

                    // TODO, we can't use apple's proposed
                    // .Apple Symbol Fallback (filtered out below),
                    // but not having these makes us not able to render
                    // many chars. We add the symbols back in.
                    // Investigate if we can actually use the .-prefixed
                    // fallbacks somehow.
                    if let Some(descriptor) =
                        descriptors_for_family("Apple Symbols").into_iter().next()
                    {
                        fallbacks.push(descriptor.to_font(size, false))
                    };

                    // Include Menlo in the fallback list as well
                    fallbacks.insert(0, Font {
                        cg_font: menlo.copy_to_CGFont(),
                        ct_font: menlo,
                        fallbacks: Vec::new(),
                    });

                    fallbacks
                })
                .unwrap_or_else(Vec::new)
        } else {
            Vec::new()
        };

        Font { ct_font, cg_font, fallbacks }
    }
}

impl Font {
    /// The the bounding rect of a glyph
    pub fn bounding_rect_for_glyph(
        &self,
        orientation: FontOrientation,
        index: u32,
    ) -> Rect<f64, ()> {
        let cg_rect = self
            .ct_font
            .get_bounding_rects_for_glyphs(orientation as CTFontOrientation, &[index as CGGlyph]);

        Rect::new(
            Point2D::new(cg_rect.origin.x, cg_rect.origin.y),
            Size2D::new(cg_rect.size.width, cg_rect.size.height),
        )
    }

    pub fn metrics(&self) -> Metrics {
        let average_advance = self.glyph_advance('0');

        let ascent = self.ct_font.ascent() as f64;
        let descent = self.ct_font.descent() as f64;
        let leading = self.ct_font.leading() as f64;
        let line_height = (ascent + descent + leading + 0.5).floor();

        // Strikeout and underline metrics.
        // CoreText doesn't provide strikeout so we provide our own.
        let underline_position = (self.ct_font.underline_position() - descent) as f32;
        let underline_thickness = self.ct_font.underline_thickness() as f32;
        let strikeout_position = (line_height / 2. - descent) as f32;
        let strikeout_thickness = underline_thickness;

        Metrics {
            average_advance,
            line_height,
            descent: -(descent as f32),
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness,
        }
    }

    pub fn is_bold(&self) -> bool {
        self.ct_font.symbolic_traits().is_bold()
    }

    pub fn is_italic(&self) -> bool {
        self.ct_font.symbolic_traits().is_italic()
    }

    fn glyph_advance(&self, character: char) -> f64 {
        let index = self.glyph_index(character).unwrap();

        let indices = [index as CGGlyph];

        unsafe {
            self.ct_font.get_advances_for_glyphs(
                FontOrientation::Default as _,
                &indices[0],
                ptr::null_mut(),
                1,
            )
        }
    }

    pub fn get_glyph(
        &self,
        key_type: KeyType,
        _size: f64,
        use_thin_strokes: bool,
    ) -> Result<RasterizedGlyph, Error> {
        let glyph_index = match key_type {
            KeyType::GlyphIndex(i) => i,
            KeyType::Char(character) => {
                self.glyph_index(character).ok_or_else(|| Error::MissingGlyph(key_type))?
            },
            KeyType::Placeholder => {
                // Early return with an empty buffer for a placeholder glyph
                return Ok(RasterizedGlyph {
                    c: KeyType::Placeholder,
                    width: 0,
                    height: 0,
                    top: 0,
                    left: 0,
                    buf: Vec::new(),
                });
            },
        };

        let bounds = self.bounding_rect_for_glyph(Default::default(), glyph_index);

        let rasterized_left = bounds.origin.x.floor() as i32;
        let rasterized_width =
            (bounds.origin.x - f64::from(rasterized_left) + bounds.size.width).ceil() as u32;
        let rasterized_descent = (-bounds.origin.y).ceil() as i32;
        let rasterized_ascent = (bounds.size.height + bounds.origin.y).ceil() as i32;
        let rasterized_height = (rasterized_descent + rasterized_ascent) as u32;

        if rasterized_width == 0 || rasterized_height == 0 {
            return Ok(RasterizedGlyph {
                c: key_type,
                width: 0,
                height: 0,
                top: 0,
                left: 0,
                buf: Vec::new(),
            });
        }

        let mut cg_context = CGContext::create_bitmap_context(
            None,
            rasterized_width as usize,
            rasterized_height as usize,
            8, // bits per component
            rasterized_width as usize * 4,
            &CGColorSpace::create_device_rgb(),
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Host,
        );

        // Give the context an opaque, black background
        cg_context.set_rgb_fill_color(0.0, 0.0, 0.0, 1.0);
        let context_rect = CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(f64::from(rasterized_width), f64::from(rasterized_height)),
        );

        cg_context.fill_rect(context_rect);

        if use_thin_strokes {
            cg_context.set_font_smoothing_style(16);
        }

        cg_context.set_allows_font_smoothing(true);
        cg_context.set_should_smooth_fonts(true);
        cg_context.set_allows_font_subpixel_quantization(true);
        cg_context.set_should_subpixel_quantize_fonts(true);
        cg_context.set_allows_font_subpixel_positioning(true);
        cg_context.set_should_subpixel_position_fonts(true);
        cg_context.set_allows_antialiasing(true);
        cg_context.set_should_antialias(true);

        // Set fill color to white for drawing the glyph
        cg_context.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
        let rasterization_origin =
            CGPoint { x: f64::from(-rasterized_left), y: f64::from(rasterized_descent) };

        self.ct_font.draw_glyphs(
            &[glyph_index as CGGlyph],
            &[rasterization_origin],
            cg_context.clone(),
        );

        let rasterized_pixels = cg_context.data().to_vec();

        let buf = extract_rgb(&rasterized_pixels);

        Ok(RasterizedGlyph {
            c: key_type,
            left: rasterized_left,
            top: (bounds.size.height + bounds.origin.y).ceil() as i32,
            width: rasterized_width as i32,
            height: rasterized_height as i32,
            buf,
        })
    }

    fn glyph_index(&self, character: char) -> Option<u32> {
        // encode this char as utf-16
        let mut buf = [0; 2];
        let encoded: &[u16] = character.encode_utf16(&mut buf);
        // and use the utf-16 buffer to get the index
        self.glyph_index_utf16(encoded)
    }

    fn glyph_index_utf16(&self, encoded: &[u16]) -> Option<u32> {
        // output buffer for the glyph. for non-BMP glyphs, like
        // emojis, this will be filled with two chars the second
        // always being a 0.
        let mut glyphs: [CGGlyph; 2] = [0; 2];

        let res = unsafe {
            self.ct_font.get_glyphs_for_characters(
                encoded.as_ptr(),
                glyphs.as_mut_ptr(),
                encoded.len() as CFIndex,
            )
        };

        if res {
            Some(u32::from(glyphs[0]))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn get_family_names() {
        let names = super::get_family_names();
        assert!(names.contains(&String::from("Menlo")));
        assert!(names.contains(&String::from("Monaco")));
    }

    #[test]
    fn get_descriptors_and_build_font() {
        let list = super::descriptors_for_family("Menlo");
        assert!(!list.is_empty());
        println!("{:?}", list);

        // Check to_font
        let fonts = list.iter().map(|desc| desc.to_font(72., false)).collect::<Vec<_>>();

        for font in fonts {
            // Get a glyph
            for c in &['a', 'b', 'c', 'd'] {
                let glyph = font.get_glyph((*c).into(), 72., false).unwrap();

                // Debug the glyph.. sigh
                for row in 0..glyph.height {
                    for col in 0..glyph.width {
                        let index = ((glyph.width * 3 * row) + (col * 3)) as usize;
                        let value = glyph.buf[index];
                        let c = match value {
                            0..=50 => ' ',
                            51..=100 => '.',
                            101..=150 => '~',
                            151..=200 => '*',
                            201..=255 => '#',
                        };
                        print!("{}", c);
                    }
                    println!();
                }
            }
        }
    }
}
