// Copyright 2019 Joe Wilm, The Alacritty Project Contributors
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
//! Rasterization powered by DirectWrite
extern crate dwrote;
use self::dwrote::{
    FontCollection, FontStretch, FontStyle, FontWeight, GlyphOffset, GlyphRunAnalysis,
};

use super::{
    FontDesc, FontKey, GlyphKey, KeyType, Metrics, RasterizedGlyph, Size, Slant, Style, Weight,
};

pub struct DirectWriteRasterizer {
    fonts: Vec<dwrote::FontFace>,
    device_pixel_ratio: f32,
}

impl crate::Rasterize for DirectWriteRasterizer {
    type Err = Error;

    fn new(device_pixel_ratio: f32, _: bool, _: bool) -> Result<DirectWriteRasterizer, Error> {
        Ok(DirectWriteRasterizer { fonts: Vec::new(), device_pixel_ratio })
    }

    fn metrics(&self, key: FontKey, size: Size) -> Result<Metrics, Error> {
        let font = self.fonts.get(key.token as usize).ok_or(Error::FontNotLoaded)?;

        let vmetrics = font.metrics();
        let scale = (size.as_f32_pts() * self.device_pixel_ratio * (96.0 / 72.0))
            / f32::from(vmetrics.designUnitsPerEm);

        let underline_position = f32::from(vmetrics.underlinePosition) * scale;
        let underline_thickness = f32::from(vmetrics.underlineThickness) * scale;

        let strikeout_position = f32::from(vmetrics.strikethroughPosition) * scale;
        let strikeout_thickness = f32::from(vmetrics.strikethroughThickness) * scale;

        let ascent = f32::from(vmetrics.ascent) * scale;
        let descent = -f32::from(vmetrics.descent) * scale;
        let line_gap = f32::from(vmetrics.lineGap) * scale;

        let line_height = f64::from(ascent - descent + line_gap);

        // We assume that all monospace characters have the same width
        // Because of this we take '!', the first drawable character, for measurements
        let glyph_metrics = font.get_design_glyph_metrics(&[33], false);
        let hmetrics = glyph_metrics.first().ok_or(Error::MissingGlyph('!'))?;

        let average_advance = f64::from(hmetrics.advanceWidth) * f64::from(scale);

        Ok(Metrics {
            descent,
            average_advance,
            line_height,
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_thickness,
        })
    }

    fn load_font(&mut self, desc: &FontDesc, _size: Size) -> Result<FontKey, Error> {
        let system_fc = FontCollection::system();

        let family = system_fc
            .get_font_family_by_name(&desc.name)
            .ok_or_else(|| Error::MissingFont(desc.clone()))?;

        let font = match desc.style {
            Style::Description { weight, slant } => {
                let weight =
                    if weight == Weight::Bold { FontWeight::Bold } else { FontWeight::Regular };

                let style = match slant {
                    Slant::Normal => FontStyle::Normal,
                    Slant::Oblique => FontStyle::Oblique,
                    Slant::Italic => FontStyle::Italic,
                };

                // This searches for the "best" font - should mean we don't have to worry about
                // fallbacks if our exact desired weight/style isn't available
                Ok(family.get_first_matching_font(weight, FontStretch::Normal, style))
            },
            Style::Specific(ref style) => {
                let mut idx = 0;
                let count = family.get_font_count();

                loop {
                    if idx == count {
                        break Err(Error::MissingFont(desc.clone()));
                    }

                    let font = family.get_font(idx);

                    if font.face_name() == *style {
                        break Ok(font);
                    }

                    idx += 1;
                }
            },
        }?;

        let face = font.create_font_face();
        self.fonts.push(face);

        Ok(FontKey { token: (self.fonts.len() - 1) as u16 })
    }

    fn get_glyph(&mut self, glyph: GlyphKey) -> Result<RasterizedGlyph, Error> {
        let font = self.fonts.get(glyph.font_key.token as usize).ok_or(Error::FontNotLoaded)?;

        let offset = GlyphOffset { advanceOffset: 0.0, ascenderOffset: 0.0 };

        let glyph_char: char = match glyph.id {
            KeyType::Char(c) => c,
            KeyType::Placeholder => ' ',
            // We never send a glyph index on Windows so this variant can't show up
            KeyType::GlyphIndex(i) => unreachable!(),
        };

        let glyph_index: u16 = *font
            .get_glyph_indices(&[glyph_char as u32])
            .first()
            .filter(|index| **index != 0)
            .ok_or_else(|| Error::MissingGlyph(glyph_char));

        let glyph_run = dwrote::DWRITE_GLYPH_RUN {
            fontFace: unsafe { font.as_ptr() },
            fontEmSize: glyph.size.as_f32_pts(),
            glyphCount: 1,
            glyphIndices: &(glyph_index),
            glyphAdvances: &(0.0),
            glyphOffsets: &(offset),
            isSideways: 0,
            bidiLevel: 0,
        };

        let rendering_mode = font.get_recommended_rendering_mode_default_params(
            glyph.size.as_f32_pts(),
            self.device_pixel_ratio * (96.0 / 72.0),
            dwrote::DWRITE_MEASURING_MODE_NATURAL,
        );

        let glyph_analysis = GlyphRunAnalysis::create(
            &glyph_run,
            self.device_pixel_ratio * (96.0 / 72.0),
            None,
            rendering_mode,
            dwrote::DWRITE_MEASURING_MODE_NATURAL,
            0.0,
            0.0,
        )
        // Since we don't shape on windows our KeyType will always be a char
        .or_else(|_| Err(Error::MissingGlyph(glyph_char)))?;

        let bounds = glyph_analysis
            .get_alpha_texture_bounds(dwrote::DWRITE_TEXTURE_CLEARTYPE_3x1)
            .or_else(|_| Err(Error::MissingGlyph(glyph_char)))?;
        let buf = glyph_analysis
            .create_alpha_texture(dwrote::DWRITE_TEXTURE_CLEARTYPE_3x1, bounds)
            .or_else(|_| Err(Error::MissingGlyph(glyph_char)))?;

        Ok(RasterizedGlyph {
            c: glyph.id,
            width: (bounds.right - bounds.left) as i32,
            height: (bounds.bottom - bounds.top) as i32,
            top: -bounds.top,
            left: bounds.left,
            buf,
        })
    }

    fn update_dpr(&mut self, device_pixel_ratio: f32) {
        self.device_pixel_ratio = device_pixel_ratio;
    }
}

#[derive(Debug)]
pub enum Error {
    MissingFont(FontDesc),
    MissingGlyph(char),
    FontNotLoaded,
}

impl ::std::error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::MissingFont(ref _desc) => "Couldn't find the requested font",
            Error::MissingGlyph(ref _c) => "Couldn't find the requested glyph",
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
