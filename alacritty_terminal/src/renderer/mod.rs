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
use std::collections::HashMap;
use std::fs::File;
use std::hash::BuildHasherDefault;
use std::io::{self, Read};
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr;
use std::sync::mpsc;
use std::time::Duration;

use fnv::FnvHasher;
#[cfg(not(any(target_os = "macos", windows)))]
use font::HbFtExt;
use font::{
    self, FontDesc, FontKey, GlyphKey, KeyType, Rasterize, RasterizedGlyph, Rasterizer,
    PLACEHOLDER_GLYPH,
};
use log::{error, info};
use notify::{watcher, DebouncedEvent, RecursiveMode, Watcher};

use crate::config::{self, Config, Delta, Font, StartupMode};
use crate::cursor::{get_cursor_glyph, CursorKey};
use crate::gl;
use crate::gl::types::*;
use crate::index::{Column, Line};
use crate::renderer::rects::RenderRect;
use crate::term::cell::{Flags, MAX_ZEROWIDTH_CHARS};
use crate::term::color::Rgb;
use crate::term::SizeInfo;
use crate::term::{self, RenderableCell};
use crate::text_run::{TextRun, TextRunContent};
use crate::util;

pub mod rects;

// Shader paths for live reload
static TEXT_SHADER_F_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../res/text.f.glsl");
static TEXT_SHADER_V_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../res/text.v.glsl");
static RECT_SHADER_F_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../res/rect.f.glsl");
static RECT_SHADER_V_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../res/rect.v.glsl");

// Shader source which is used when live-shader-reload feature is disable
static TEXT_SHADER_F: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../res/text.f.glsl"));
static TEXT_SHADER_V: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../res/text.v.glsl"));
static RECT_SHADER_F: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../res/rect.f.glsl"));
static RECT_SHADER_V: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../res/rect.v.glsl"));

/// `LoadGlyph` allows for copying a rasterized glyph into graphics memory
pub trait LoadGlyph {
    /// Load the rasterized glyph into GPU memory
    fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Glyph;

    /// Clear any state accumulated from previous loaded glyphs
    ///
    /// This can, for instance, be used to reset the texture Atlas.
    fn clear(&mut self);
}

enum Msg {
    ShaderReload,
}

#[derive(Debug)]
pub enum Error {
    ShaderCreation(ShaderCreationError),
}

impl ::std::error::Error for Error {
    fn cause(&self) -> Option<&dyn (::std::error::Error)> {
        match *self {
            Error::ShaderCreation(ref err) => Some(err),
        }
    }

    fn description(&self) -> &str {
        match *self {
            Error::ShaderCreation(ref err) => err.description(),
        }
    }
}

impl ::std::fmt::Display for Error {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            Error::ShaderCreation(ref err) => {
                write!(f, "There was an error initializing the shaders: {}", err)
            },
        }
    }
}

impl From<ShaderCreationError> for Error {
    fn from(val: ShaderCreationError) -> Error {
        Error::ShaderCreation(val)
    }
}

/// Text drawing program
///
/// Uniforms are prefixed with "u", and vertex attributes are prefixed with "a".
#[derive(Debug)]
pub struct TextShaderProgram {
    // Program id
    id: GLuint,

    /// projection scale and offset uniform
    u_projection: GLint,

    /// Cell dimensions (pixels)
    u_cell_dim: GLint,

    /// Background pass flag
    ///
    /// Rendering is split into two passes; 1 for backgrounds, and one for text
    u_background: GLint,
}

/// Rectangle drawing program
///
/// Uniforms are prefixed with "u"
#[derive(Debug)]
pub struct RectShaderProgram {
    // Program id
    id: GLuint,
    /// Rectangle color
    u_color: GLint,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Glyph {
    tex_id: GLuint,
    top: f32,
    left: f32,
    width: f32,
    height: f32,
    uv_bot: f32,
    uv_left: f32,
    uv_width: f32,
    uv_height: f32,
}

/// Naïve glyph cache
///
/// Currently only keyed by `char`, and thus not possible to hold different
/// representations of the same code point.
pub struct GlyphCache {
    /// Cache of buffered glyphs
    cache: HashMap<GlyphKey, Glyph, BuildHasherDefault<FnvHasher>>,

    /// Cache of buffered cursor glyphs
    cursor_cache: HashMap<CursorKey, Glyph, BuildHasherDefault<FnvHasher>>,

    /// Rasterizer for loading new glyphs
    rasterizer: Rasterizer,

    /// regular font
    font_key: FontKey,

    /// bold font
    bold_key: FontKey,

    /// italic font
    italic_key: FontKey,

    /// bold italic font
    bold_italic_key: FontKey,

    /// font size
    font_size: font::Size,

    /// glyph offset
    glyph_offset: Delta<i8>,

    metrics: ::font::Metrics,
}

impl GlyphCache {
    pub fn new<L>(
        mut rasterizer: Rasterizer,
        font: &config::Font,
        loader: &mut L,
    ) -> Result<GlyphCache, font::Error>
    where
        L: LoadGlyph,
    {
        let (regular, bold, italic, bold_italic) = Self::compute_font_keys(font, &mut rasterizer)?;

        // Need to load at least one glyph for the face before calling metrics.
        // The glyph requested here has no special meaning.
        rasterizer.get_glyph(GlyphKey {
            id: PLACEHOLDER_GLYPH,
            font_key: regular,
            size: font.size,
        })?;

        let metrics = rasterizer.metrics(regular, font.size)?;

        let mut cache = GlyphCache {
            cache: HashMap::default(),
            cursor_cache: HashMap::default(),
            rasterizer,
            font_size: font.size,
            font_key: regular,
            bold_key: bold,
            italic_key: italic,
            bold_italic_key: bold_italic,
            glyph_offset: font.glyph_offset,
            metrics,
        };

        cache.load_glyphs_for_font(regular, loader);
        cache.load_glyphs_for_font(bold, loader);
        cache.load_glyphs_for_font(italic, loader);
        cache.load_glyphs_for_font(bold_italic, loader);

        Ok(cache)
    }

    fn load_glyphs_for_font<L: LoadGlyph>(&mut self, font: FontKey, loader: &mut L) {
        let size = self.font_size;
        for i in 32..=126 {
            self.get(GlyphKey { font_key: font, id: KeyType::GlyphIndex(i), size }, loader);
        }
    }

    /// Computes font keys for (Regular, Bold, Italic, Bold Italic)
    fn compute_font_keys(
        font: &config::Font,
        rasterizer: &mut Rasterizer,
    ) -> Result<(FontKey, FontKey, FontKey, FontKey), font::Error> {
        let size = font.size;

        // Load regular font
        let regular_desc =
            Self::make_desc(&font.normal(), font::Slant::Normal, font::Weight::Normal);

        let regular = rasterizer.load_font(&regular_desc, size)?;

        // helper to load a description if it is not the regular_desc
        let mut load_or_regular = |desc: FontDesc| {
            if desc == regular_desc {
                regular
            } else {
                rasterizer.load_font(&desc, size).unwrap_or_else(|_| regular)
            }
        };

        // Load bold font
        let bold_desc = Self::make_desc(&font.bold(), font::Slant::Normal, font::Weight::Bold);

        let bold = load_or_regular(bold_desc);

        // Load italic font
        let italic_desc =
            Self::make_desc(&font.italic(), font::Slant::Italic, font::Weight::Normal);

        let italic = load_or_regular(italic_desc);

        // Load bold italic font
        let bold_italic_desc =
            Self::make_desc(&font.bold_italic(), font::Slant::Italic, font::Weight::Bold);

        let bold_italic = load_or_regular(bold_italic_desc);

        Ok((regular, bold, italic, bold_italic))
    }

    fn make_desc(
        desc: &config::FontDescription,
        slant: font::Slant,
        weight: font::Weight,
    ) -> FontDesc {
        let style = if let Some(ref spec) = desc.style {
            font::Style::Specific(spec.to_owned())
        } else {
            font::Style::Description { slant, weight }
        };
        FontDesc::new(desc.family.clone(), style)
    }

    //pub fn font_metrics(&self) -> font::Metrics {
    //    self.rasterizer
    //        .metrics(self.font_key, self.font_size)
    //        .expect("metrics load since font is loaded at glyph cache creation")
    //}

    // Since shaping is not available on Windows/Mac OSX, text run glyphs are rasterized one by one
    // as characters
    #[cfg(any(target_os = "macos", windows))]
    fn shape_run<'a, L>(
        &'a mut self,
        text_run: &str,
        font_key: FontKey,
        loader: &'a mut L,
    ) -> Vec<Glyph>
    where
        L: LoadGlyph,
    {
        text_run
            .chars()
            .map(|c| {
                let glyph_key = GlyphKey { id: c.into(), font_key, size: self.font_size };
                *self.get(glyph_key, loader)
            })
            .collect()
    }

    // Shape using harfbuzz
    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn shape_run<'a, L>(
        &'a mut self,
        text_run: &'a str,
        font_key: FontKey,
        loader: &'a mut L,
    ) -> Vec<Glyph>
    where
        L: LoadGlyph,
    {
        self.rasterizer
            .shape(text_run, font_key)
            .get_glyph_infos()
            .iter()
            .map(move |glyph_info| {
                let codepoint = glyph_info.codepoint;

                // Codepoint of 0 indicates a missing or undefined glyph
                let id: font::KeyType = if codepoint == 0 {
                    Self::find_fallback_char(text_run, glyph_info.cluster as usize)
                } else {
                    codepoint.into()
                };

                let glyph_key = GlyphKey { id, font_key, size: self.font_size };
                *self.get(glyph_key, loader)
            })
            .collect()
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn find_fallback_char(text_run: &str, index: usize) -> font::KeyType {
        // TODO: this is a linear scan over text_run for each missing glyph.
        // Try to find all missing glyphs first and only scan over text_run once.
        text_run
            .char_indices()
            .find_map(|(i, c)| if i == index { Some(c) } else { None })
            .unwrap_or_else(|| panic!("Could not find cluster {} in run {}", index, text_run))
            .into()
    }

    pub fn get<L>(&mut self, glyph_key: GlyphKey, loader: &mut L) -> &Glyph
    where
        L: LoadGlyph,
    {
        let glyph_offset = self.glyph_offset;
        let rasterizer = &mut self.rasterizer;
        let metrics = &self.metrics;
        self.cache.entry(glyph_key).or_insert_with(|| {
            let mut rasterized =
                rasterizer.get_glyph(glyph_key).unwrap_or_else(|_| Default::default());

            rasterized.left += i32::from(glyph_offset.x);
            rasterized.top += i32::from(glyph_offset.y);
            rasterized.top -= metrics.descent as i32;

            loader.load_glyph(&rasterized)
        })
    }

    pub fn update_font_size<L: LoadGlyph>(
        &mut self,
        font: config::Font,
        dpr: f64,
        loader: &mut L,
    ) -> Result<(), font::Error> {
        // Clear currently cached data in both GL and the registry
        loader.clear();
        self.cache = HashMap::default();
        self.cursor_cache = HashMap::default();

        // Update dpi scaling
        self.rasterizer.update_dpr(dpr as f32);

        // Recompute font keys
        let (regular, bold, italic, bold_italic) =
            Self::compute_font_keys(&font, &mut self.rasterizer)?;

        self.rasterizer.get_glyph(GlyphKey {
            id: PLACEHOLDER_GLYPH,
            font_key: regular,
            size: font.size,
        })?;
        let metrics = self.rasterizer.metrics(regular, font.size)?;

        info!("Font size changed to {:?} with DPR of {}", font.size, dpr);

        self.font_size = font.size;
        self.font_key = regular;
        self.bold_key = bold;
        self.italic_key = italic;
        self.bold_italic_key = bold_italic;
        self.metrics = metrics;

        self.load_glyphs_for_font(regular, loader);
        self.load_glyphs_for_font(bold, loader);
        self.load_glyphs_for_font(italic, loader);
        self.load_glyphs_for_font(bold_italic, loader);

        Ok(())
    }

    pub fn font_metrics(&self) -> font::Metrics {
        self.rasterizer
            .metrics(self.font_key, self.font_size)
            .expect("metrics load since font is loaded at glyph cache creation")
    }

    // Calculate font metrics without access to a glyph cache
    pub fn static_metrics(font: Font, dpr: f64) -> Result<font::Metrics, font::Error> {
        let mut rasterizer = font::Rasterizer::new(dpr as f32, font.use_thin_strokes(), font.ligatures())?;
        let regular_desc =
            GlyphCache::make_desc(&font.normal(), font::Slant::Normal, font::Weight::Normal);
        let regular = rasterizer.load_font(&regular_desc, font.size)?;
        rasterizer.get_glyph(GlyphKey {
            id: PLACEHOLDER_GLYPH,
            font_key: regular,
            size: font.size,
        })?;

        rasterizer.metrics(regular, font.size)
    }

    pub fn calculate_dimensions<C>(
        config: &Config<C>,
        dpr: f64,
        cell_width: f32,
        cell_height: f32,
    ) -> Option<(f64, f64)> {
        let dimensions = config.window.dimensions;

        if dimensions.columns_u32() == 0
            || dimensions.lines_u32() == 0
            || config.window.startup_mode() != StartupMode::Windowed
        {
            return None;
        }

        let padding_x = f64::from(config.window.padding.x) * dpr;
        let padding_y = f64::from(config.window.padding.y) * dpr;

        // Calculate new size based on cols/lines specified in config
        let grid_width = cell_width as u32 * dimensions.columns_u32();
        let grid_height = cell_height as u32 * dimensions.lines_u32();

        let width = (f64::from(grid_width) + 2. * padding_x).floor();
        let height = (f64::from(grid_height) + 2. * padding_y).floor();

        Some((width, height))
    }
}

#[derive(Debug)]
#[repr(C)]
struct InstanceData {
    // coords
    col: f32,
    row: f32,
    // glyph offset
    left: f32,
    top: f32,
    // glyph scale
    width: f32,
    height: f32,
    // uv offset
    uv_left: f32,
    uv_bot: f32,
    // uv scale
    uv_width: f32,
    uv_height: f32,
    // color
    r: f32,
    g: f32,
    b: f32,
    // background color
    bg_r: f32,
    bg_g: f32,
    bg_b: f32,
    bg_a: f32,
}

#[derive(Debug)]
pub struct QuadRenderer {
    program: TextShaderProgram,
    rect_program: RectShaderProgram,
    vao: GLuint,
    ebo: GLuint,
    vbo_instance: GLuint,
    rect_vao: GLuint,
    rect_vbo: GLuint,
    atlas: Vec<Atlas>,
    current_atlas: usize,
    active_tex: GLuint,
    batch: Batch,
    rx: mpsc::Receiver<Msg>,
}

#[derive(Debug)]
pub struct RenderApi<'a, C> {
    active_tex: &'a mut GLuint,
    batch: &'a mut Batch,
    atlas: &'a mut Vec<Atlas>,
    current_atlas: &'a mut usize,
    program: &'a mut TextShaderProgram,
    config: &'a Config<C>,
}

#[derive(Debug)]
pub struct LoaderApi<'a> {
    active_tex: &'a mut GLuint,
    atlas: &'a mut Vec<Atlas>,
    current_atlas: &'a mut usize,
}

#[derive(Debug)]
pub struct PackedVertex {
    x: f32,
    y: f32,
}

#[derive(Debug, Default)]
pub struct Batch {
    tex: GLuint,
    instances: Vec<InstanceData>,
}

impl Batch {
    #[inline]
    pub fn new() -> Batch {
        Batch { tex: 0, instances: Vec::with_capacity(BATCH_MAX) }
    }

    pub fn add_item(&mut self, cell: RenderableCell, glyph: &Glyph) {
        if self.is_empty() {
            self.tex = glyph.tex_id;
        }

        self.instances.push(InstanceData {
            col: cell.column.0 as f32,
            row: cell.line.0 as f32,

            top: glyph.top,
            left: glyph.left,
            width: glyph.width,
            height: glyph.height,

            uv_bot: glyph.uv_bot,
            uv_left: glyph.uv_left,
            uv_width: glyph.uv_width,
            uv_height: glyph.uv_height,

            r: f32::from(cell.fg.r),
            g: f32::from(cell.fg.g),
            b: f32::from(cell.fg.b),

            bg_r: f32::from(cell.bg.r),
            bg_g: f32::from(cell.bg.g),
            bg_b: f32::from(cell.bg.b),
            bg_a: cell.bg_alpha,
        });
    }

    #[inline]
    pub fn full(&self) -> bool {
        self.capacity() == self.len()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.instances.len()
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        BATCH_MAX
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.len() * size_of::<InstanceData>()
    }

    pub fn clear(&mut self) {
        self.tex = 0;
        self.instances.clear();
    }
}

/// Maximum items to be drawn in a batch.
const BATCH_MAX: usize = 0x1_0000;
const ATLAS_SIZE: i32 = 1024;

impl QuadRenderer {
    pub fn new() -> Result<QuadRenderer, Error> {
        let program = TextShaderProgram::new()?;
        let rect_program = RectShaderProgram::new()?;

        let mut vao: GLuint = 0;
        let mut ebo: GLuint = 0;

        let mut vbo_instance: GLuint = 0;

        let mut rect_vao: GLuint = 0;
        let mut rect_vbo: GLuint = 0;
        let mut rect_ebo: GLuint = 0;

        unsafe {
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);
            gl::Enable(gl::MULTISAMPLE);

            // Disable depth mask, as the renderer never uses depth tests
            gl::DepthMask(gl::FALSE);

            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut ebo);
            gl::GenBuffers(1, &mut vbo_instance);
            gl::BindVertexArray(vao);

            // ---------------------
            // Set up element buffer
            // ---------------------
            let indices: [u32; 6] = [0, 1, 3, 1, 2, 3];

            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, ebo);
            gl::BufferData(
                gl::ELEMENT_ARRAY_BUFFER,
                (6 * size_of::<u32>()) as isize,
                indices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // ----------------------------
            // Setup vertex instance buffer
            // ----------------------------
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo_instance);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (BATCH_MAX * size_of::<InstanceData>()) as isize,
                ptr::null(),
                gl::STREAM_DRAW,
            );
            // coords
            gl::VertexAttribPointer(
                0,
                2,
                gl::FLOAT,
                gl::FALSE,
                size_of::<InstanceData>() as i32,
                ptr::null(),
            );
            gl::EnableVertexAttribArray(0);
            gl::VertexAttribDivisor(0, 1);
            // glyphoffset
            gl::VertexAttribPointer(
                1,
                4,
                gl::FLOAT,
                gl::FALSE,
                size_of::<InstanceData>() as i32,
                (2 * size_of::<f32>()) as *const _,
            );
            gl::EnableVertexAttribArray(1);
            gl::VertexAttribDivisor(1, 1);
            // uv
            gl::VertexAttribPointer(
                2,
                4,
                gl::FLOAT,
                gl::FALSE,
                size_of::<InstanceData>() as i32,
                (6 * size_of::<f32>()) as *const _,
            );
            gl::EnableVertexAttribArray(2);
            gl::VertexAttribDivisor(2, 1);
            // color
            gl::VertexAttribPointer(
                3,
                3,
                gl::FLOAT,
                gl::FALSE,
                size_of::<InstanceData>() as i32,
                (10 * size_of::<f32>()) as *const _,
            );
            gl::EnableVertexAttribArray(3);
            gl::VertexAttribDivisor(3, 1);
            // color
            gl::VertexAttribPointer(
                4,
                4,
                gl::FLOAT,
                gl::FALSE,
                size_of::<InstanceData>() as i32,
                (13 * size_of::<f32>()) as *const _,
            );
            gl::EnableVertexAttribArray(4);
            gl::VertexAttribDivisor(4, 1);

            // Rectangle setup
            gl::GenVertexArrays(1, &mut rect_vao);
            gl::GenBuffers(1, &mut rect_vbo);
            gl::GenBuffers(1, &mut rect_ebo);
            gl::BindVertexArray(rect_vao);
            let indices: [i32; 6] = [0, 1, 3, 1, 2, 3];
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, rect_ebo);
            gl::BufferData(
                gl::ELEMENT_ARRAY_BUFFER,
                (size_of::<i32>() * indices.len()) as _,
                indices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // Cleanup
            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, 0);
        }

        let (msg_tx, msg_rx) = mpsc::channel();

        if cfg!(feature = "live-shader-reload") {
            util::thread::spawn_named("live shader reload", move || {
                let (tx, rx) = ::std::sync::mpsc::channel();
                // The Duration argument is a debouncing period.
                let mut watcher =
                    watcher(tx, Duration::from_millis(10)).expect("create file watcher");
                watcher
                    .watch(TEXT_SHADER_F_PATH, RecursiveMode::NonRecursive)
                    .expect("watch fragment shader");
                watcher
                    .watch(TEXT_SHADER_V_PATH, RecursiveMode::NonRecursive)
                    .expect("watch vertex shader");

                loop {
                    let event = rx.recv().expect("watcher event");

                    match event {
                        DebouncedEvent::Rename(..) => continue,
                        DebouncedEvent::Create(_)
                        | DebouncedEvent::Write(_)
                        | DebouncedEvent::Chmod(_) => {
                            msg_tx.send(Msg::ShaderReload).expect("msg send ok");
                        },
                        _ => {},
                    }
                }
            });
        }

        let mut renderer = QuadRenderer {
            program,
            rect_program,
            vao,
            ebo,
            vbo_instance,
            rect_vao,
            rect_vbo,
            atlas: Vec::new(),
            current_atlas: 0,
            active_tex: 0,
            batch: Batch::new(),
            rx: msg_rx,
        };

        let atlas = Atlas::new(ATLAS_SIZE);
        renderer.atlas.push(atlas);

        Ok(renderer)
    }

    // Draw all rectangles simultaneously to prevent excessive program swaps
    pub fn draw_rects(
        &mut self,
        props: &term::SizeInfo,
        visual_bell_color: Rgb,
        visual_bell_intensity: f64,
        cell_line_rects: Vec<RenderRect>,
    ) {
        // Swap to rectangle rendering program
        unsafe {
            // Swap program
            gl::UseProgram(self.rect_program.id);

            // Remove padding from viewport
            gl::Viewport(0, 0, props.width as i32, props.height as i32);

            // Change blending strategy
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);

            // Setup data and buffers
            gl::BindVertexArray(self.rect_vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.rect_vbo);

            // Position
            gl::VertexAttribPointer(
                0,
                2,
                gl::FLOAT,
                gl::FALSE,
                (size_of::<f32>() * 2) as _,
                ptr::null(),
            );
            gl::EnableVertexAttribArray(0);
        }

        // Draw visual bell
        let rect = RenderRect::new(0., 0., props.width, props.height, visual_bell_color);
        self.render_rect(&rect, visual_bell_intensity as f32, props);

        // Draw underlines and strikeouts
        for cell_line_rect in cell_line_rects {
            self.render_rect(&cell_line_rect, 255., props);
        }

        // Deactivate rectangle program again
        unsafe {
            // Reset blending strategy
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);

            // Reset data and buffers
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);

            let padding_x = props.padding_x as i32;
            let padding_y = props.padding_y as i32;
            let width = props.width as i32;
            let height = props.height as i32;
            gl::Viewport(padding_x, padding_y, width - 2 * padding_x, height - 2 * padding_y);

            // Disable program
            gl::UseProgram(0);
        }
    }

    pub fn with_api<F, T, C>(&mut self, config: &Config<C>, props: &term::SizeInfo, func: F) -> T
    where
        F: FnOnce(RenderApi<'_, C>) -> T,
    {
        // Flush message queue
        if let Ok(Msg::ShaderReload) = self.rx.try_recv() {
            self.reload_shaders(props);
        }
        while let Ok(_) = self.rx.try_recv() {}

        unsafe {
            gl::UseProgram(self.program.id);
            self.program.set_term_uniforms(props);

            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, self.ebo);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo_instance);
            gl::ActiveTexture(gl::TEXTURE0);
        }

        let res = func(RenderApi {
            active_tex: &mut self.active_tex,
            batch: &mut self.batch,
            atlas: &mut self.atlas,
            current_atlas: &mut self.current_atlas,
            program: &mut self.program,
            config,
        });

        unsafe {
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, 0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);

            gl::UseProgram(0);
        }

        res
    }

    pub fn with_loader<F, T>(&mut self, func: F) -> T
    where
        F: FnOnce(LoaderApi<'_>) -> T,
    {
        unsafe {
            gl::ActiveTexture(gl::TEXTURE0);
        }

        func(LoaderApi {
            active_tex: &mut self.active_tex,
            atlas: &mut self.atlas,
            current_atlas: &mut self.current_atlas,
        })
    }

    pub fn reload_shaders(&mut self, props: &term::SizeInfo) {
        info!("Reloading shaders...");
        let result = (TextShaderProgram::new(), RectShaderProgram::new());
        let (program, rect_program) = match result {
            (Ok(program), Ok(rect_program)) => {
                unsafe {
                    gl::UseProgram(program.id);
                    program.update_projection(
                        props.width,
                        props.height,
                        props.padding_x,
                        props.padding_y,
                    );
                    gl::UseProgram(0);
                }

                info!("... successfully reloaded shaders");
                (program, rect_program)
            },
            (Err(err), _) | (_, Err(err)) => {
                error!("{}", err);
                return;
            },
        };

        self.active_tex = 0;
        self.program = program;
        self.rect_program = rect_program;
    }

    pub fn resize(&mut self, size: &SizeInfo) {
        // viewport
        unsafe {
            gl::Viewport(
                size.padding_x as i32,
                size.padding_y as i32,
                size.width as i32 - 2 * size.padding_x as i32,
                size.height as i32 - 2 * size.padding_y as i32,
            );

            // update projection
            gl::UseProgram(self.program.id);
            self.program.update_projection(size.width, size.height, size.padding_x, size.padding_y);
            gl::UseProgram(0);
        }
    }

    // Render a rectangle
    //
    // This requires the rectangle program to be activated
    fn render_rect(&mut self, rect: &RenderRect, alpha: f32, size: &term::SizeInfo) {
        // Do nothing when alpha is fully transparent
        if alpha == 0. {
            return;
        }

        // Calculate rectangle position
        let center_x = size.width / 2.;
        let center_y = size.height / 2.;
        let x = (rect.x - center_x) / center_x;
        let y = -(rect.y - center_y) / center_y;
        let width = rect.width / center_x;
        let height = rect.height / center_y;

        unsafe {
            // Setup vertices
            let vertices: [f32; 8] = [x + width, y, x + width, y - height, x, y - height, x, y];

            // Load vertex data into array buffer
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (size_of::<f32>() * vertices.len()) as _,
                vertices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // Color
            self.rect_program.set_color(rect.color, alpha);

            // Draw the rectangle
            gl::DrawElements(gl::TRIANGLES, 6, gl::UNSIGNED_INT, ptr::null());
        }
    }
}

impl<'a, C> RenderApi<'a, C> {
    pub fn clear(&self, color: Rgb) {
        unsafe {
            let alpha = self.config.background_opacity();
            gl::ClearColor(
                (f32::from(color.r) / 255.0).min(1.0) * alpha,
                (f32::from(color.g) / 255.0).min(1.0) * alpha,
                (f32::from(color.b) / 255.0).min(1.0) * alpha,
                alpha,
            );
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
    }

    fn render_batch(&mut self) {
        unsafe {
            gl::BufferSubData(
                gl::ARRAY_BUFFER,
                0,
                self.batch.size() as isize,
                self.batch.instances.as_ptr() as *const _,
            );
        }

        // Bind texture if necessary
        if *self.active_tex != self.batch.tex {
            unsafe {
                gl::BindTexture(gl::TEXTURE_2D, self.batch.tex);
            }
            *self.active_tex = self.batch.tex;
        }

        unsafe {
            self.program.set_background_pass(true);
            gl::DrawElementsInstanced(
                gl::TRIANGLES,
                6,
                gl::UNSIGNED_INT,
                ptr::null(),
                self.batch.len() as GLsizei,
            );
            self.program.set_background_pass(false);
            gl::DrawElementsInstanced(
                gl::TRIANGLES,
                6,
                gl::UNSIGNED_INT,
                ptr::null(),
                self.batch.len() as GLsizei,
            );
        }

        self.batch.clear();
    }

    /// Render a string in a variable location. Used for printing the render timer, warnings and
    /// errors.
    pub fn render_string(
        &mut self,
        string: &str,
        line: Line,
        glyph_cache: &mut GlyphCache,
        color: Option<Rgb>,
    ) {
        let bg_alpha = color.map(|_| 1.0).unwrap_or(0.0);

        let str_length: usize = string.chars().map(|_| 1).sum();
        let text_run = TextRun {
            line,
            span: (Column(0), Column(str_length - 1)),
            content: TextRunContent::CharRun(string.to_owned(), vec![
                [' '; MAX_ZEROWIDTH_CHARS];
                str_length
            ]),
            fg: Rgb { r: 0, g: 0, b: 0 },
            bg: color.unwrap_or(Rgb { r: 0, g: 0, b: 0 }),
            flags: Flags::empty(),
            bg_alpha,
        };

        self.render_text_run(text_run, glyph_cache);
    }

    #[inline]
    fn add_render_item(&mut self, cell: RenderableCell, glyph: &Glyph) {
        // Flush batch if tex changing
        if !self.batch.is_empty() && self.batch.tex != glyph.tex_id {
            self.render_batch();
        }

        self.batch.add_item(cell, glyph);

        // Render batch and clear if it's full
        if self.batch.full() {
            self.render_batch();
        }
    }

    fn render_cursor(
        &mut self,
        start_cell: RenderableCell,
        cursor_key: CursorKey,
        glyph_cache: &mut GlyphCache,
    ) {
        // Raw cell pixel buffers like cursors don't need to go through font lookup
        let metrics = glyph_cache.metrics;
        let glyph = glyph_cache.cursor_cache.entry(cursor_key).or_insert_with(|| {
            self.load_glyph(&get_cursor_glyph(
                cursor_key.style,
                metrics,
                self.config.font.offset.x,
                self.config.font.offset.y,
                cursor_key.is_wide,
            ))
        });
        self.add_render_item(start_cell, &glyph);
    }

    fn determine_font_key(flags: Flags, glyph_cache: &GlyphCache) -> FontKey {
        // FIXME this is super inefficient.
        match flags & Flags::BOLD_ITALIC {
            Flags::BOLD_ITALIC => glyph_cache.bold_italic_key,
            Flags::BOLD => glyph_cache.bold_key,
            Flags::ITALIC => glyph_cache.italic_key,
            _ => glyph_cache.font_key,
        }
    }

    fn render_zero_widths<'r, I>(
        &mut self,
        zero_width_chars: I,
        cell: RenderableCell,
        font_key: FontKey,
        glyph_cache: &mut GlyphCache,
    ) where
        I: Iterator<Item = &'r char>,
    {
        for c in zero_width_chars {
            let glyph_key = GlyphKey { font_key, size: glyph_cache.font_size, id: (*c).into() };
            let average_advance = glyph_cache.metrics.average_advance as f32;
            let mut glyph = *glyph_cache.get(glyph_key, self);

            // The metrics of zero-width characters are based on rendering
            // the character after the current cell, with the anchor at the
            // right side of the preceding character. Since we render the
            // zero-width characters inside the preceding character, the
            // anchor has been moved to the right by one cell.
            glyph.left += average_advance;

            self.add_render_item(cell, &glyph);
        }
    }

    pub fn render_text_run(&mut self, text_run: TextRun, glyph_cache: &mut GlyphCache) {
        match &text_run.content {
            TextRunContent::Cursor(cursor_key) => {
                self.render_cursor(text_run.start_cell(), *cursor_key, glyph_cache)
            },
            TextRunContent::CharRun(run, zero_widths) => {
                // Get font key for cell
                let font_key = Self::determine_font_key(text_run.flags, glyph_cache);

                let shaped_glyphs = if text_run.flags.contains(Flags::HIDDEN) {
                    GlyphIter::Hidden
                } else {
                    GlyphIter::Shaped(glyph_cache.shape_run(&run, font_key, self).into_iter())
                };

                for ((cell, glyph), zero_width_chars) in
                    text_run.cells().zip(shaped_glyphs).zip(zero_widths.iter())
                {
                    self.add_render_item(cell, &glyph);
                    // Add empty spacer for full width characters
                    if text_run.flags.contains(Flags::WIDE_CHAR) {
                        self.add_render_item(
                            RenderableCell { column: cell.column + 1, ..cell },
                            &Glyph::default(),
                        );
                    }
                    self.render_zero_widths(
                        zero_width_chars.iter().filter(|c| **c != ' '),
                        cell,
                        font_key,
                        glyph_cache,
                    );
                }
            },
        };
    }
}

/// Abstracts iteration over a run of hidden glyphs or shaped glyphs.
enum GlyphIter<I> {
    /// Our run was not hidden and our glyphs were shaped
    Shaped(I),
    /// Our run is hidden and was not shaped
    Hidden,
}

impl<I> Iterator for GlyphIter<I>
where
    I: Iterator<Item = Glyph>,
{
    type Item = Glyph;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            GlyphIter::Shaped(inner) => inner.next(),
            GlyphIter::Hidden => Some(Glyph::default()),
        }
    }
}

/// Load a glyph into a texture atlas
///
/// If the current atlas is full, a new one will be created.
#[inline]
fn load_glyph(
    active_tex: &mut GLuint,
    atlas: &mut Vec<Atlas>,
    current_atlas: &mut usize,
    rasterized: &RasterizedGlyph,
) -> Glyph {
    // At least one atlas is guaranteed to be in the `self.atlas` list; thus
    // the unwrap.
    match atlas[*current_atlas].insert(rasterized, active_tex) {
        Ok(glyph) => glyph,
        Err(AtlasInsertError::Full) => {
            *current_atlas += 1;
            if *current_atlas == atlas.len() {
                let new = Atlas::new(ATLAS_SIZE);
                *active_tex = 0; // Atlas::new binds a texture. Ugh this is sloppy.
                atlas.push(new);
            }
            load_glyph(active_tex, atlas, current_atlas, rasterized)
        },
        Err(AtlasInsertError::GlyphTooLarge) => Glyph {
            tex_id: atlas[*current_atlas].id,
            top: 0.0,
            left: 0.0,
            width: 0.0,
            height: 0.0,
            uv_bot: 0.0,
            uv_left: 0.0,
            uv_width: 0.0,
            uv_height: 0.0,
        },
    }
}

#[inline]
fn clear_atlas(atlas: &mut Vec<Atlas>, current_atlas: &mut usize) {
    for atlas in atlas.iter_mut() {
        atlas.clear();
    }
    *current_atlas = 0;
}

impl<'a> LoadGlyph for LoaderApi<'a> {
    fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Glyph {
        load_glyph(self.active_tex, self.atlas, self.current_atlas, rasterized)
    }

    fn clear(&mut self) {
        clear_atlas(self.atlas, self.current_atlas)
    }
}

impl<'a, C> LoadGlyph for RenderApi<'a, C> {
    fn load_glyph(&mut self, rasterized: &RasterizedGlyph) -> Glyph {
        load_glyph(self.active_tex, self.atlas, self.current_atlas, rasterized)
    }

    fn clear(&mut self) {
        clear_atlas(self.atlas, self.current_atlas)
    }
}

impl<'a, C> Drop for RenderApi<'a, C> {
    fn drop(&mut self) {
        if !self.batch.is_empty() {
            self.render_batch();
        }
    }
}

impl TextShaderProgram {
    pub fn new() -> Result<TextShaderProgram, ShaderCreationError> {
        let (vertex_src, fragment_src) = if cfg!(feature = "live-shader-reload") {
            (None, None)
        } else {
            (Some(TEXT_SHADER_V), Some(TEXT_SHADER_F))
        };
        let vertex_shader = create_shader(TEXT_SHADER_V_PATH, gl::VERTEX_SHADER, vertex_src)?;
        let fragment_shader = create_shader(TEXT_SHADER_F_PATH, gl::FRAGMENT_SHADER, fragment_src)?;
        let program = create_program(vertex_shader, fragment_shader)?;

        unsafe {
            gl::DeleteShader(fragment_shader);
            gl::DeleteShader(vertex_shader);
            gl::UseProgram(program);
        }

        macro_rules! cptr {
            ($thing:expr) => {
                $thing.as_ptr() as *const _
            };
        }

        macro_rules! assert_uniform_valid {
            ($uniform:expr) => {
                assert!($uniform != gl::INVALID_VALUE as i32);
                assert!($uniform != gl::INVALID_OPERATION as i32);
            };
            ( $( $uniform:expr ),* ) => {
                $( assert_uniform_valid!($uniform); )*
            };
        }

        // get uniform locations
        let (projection, cell_dim, background) = unsafe {
            (
                gl::GetUniformLocation(program, cptr!(b"projection\0")),
                gl::GetUniformLocation(program, cptr!(b"cellDim\0")),
                gl::GetUniformLocation(program, cptr!(b"backgroundPass\0")),
            )
        };

        assert_uniform_valid!(projection, cell_dim, background);

        let shader = TextShaderProgram {
            id: program,
            u_projection: projection,
            u_cell_dim: cell_dim,
            u_background: background,
        };

        unsafe {
            gl::UseProgram(0);
        }

        Ok(shader)
    }

    fn update_projection(&self, width: f32, height: f32, padding_x: f32, padding_y: f32) {
        // Bounds check
        if (width as u32) < (2 * padding_x as u32) || (height as u32) < (2 * padding_y as u32) {
            return;
        }

        // Compute scale and offset factors, from pixel to ndc space. Y is inverted
        //   [0, width - 2 * padding_x] to [-1, 1]
        //   [height - 2 * padding_y, 0] to [-1, 1]
        let scale_x = 2. / (width - 2. * padding_x);
        let scale_y = -2. / (height - 2. * padding_y);
        let offset_x = -1.;
        let offset_y = 1.;

        info!("Width: {}, Height: {}", width, height);

        unsafe {
            gl::Uniform4f(self.u_projection, offset_x, offset_y, scale_x, scale_y);
        }
    }

    fn set_term_uniforms(&self, props: &term::SizeInfo) {
        unsafe {
            gl::Uniform2f(self.u_cell_dim, props.cell_width, props.cell_height);
        }
    }

    fn set_background_pass(&self, background_pass: bool) {
        let value = if background_pass { 1 } else { 0 };

        unsafe {
            gl::Uniform1i(self.u_background, value);
        }
    }
}

impl Drop for TextShaderProgram {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteProgram(self.id);
        }
    }
}

impl RectShaderProgram {
    pub fn new() -> Result<Self, ShaderCreationError> {
        let (vertex_src, fragment_src) = if cfg!(feature = "live-shader-reload") {
            (None, None)
        } else {
            (Some(RECT_SHADER_V), Some(RECT_SHADER_F))
        };
        let vertex_shader = create_shader(RECT_SHADER_V_PATH, gl::VERTEX_SHADER, vertex_src)?;
        let fragment_shader = create_shader(RECT_SHADER_F_PATH, gl::FRAGMENT_SHADER, fragment_src)?;
        let program = create_program(vertex_shader, fragment_shader)?;

        unsafe {
            gl::DeleteShader(fragment_shader);
            gl::DeleteShader(vertex_shader);
            gl::UseProgram(program);
        }

        // get uniform locations
        let u_color = unsafe { gl::GetUniformLocation(program, b"color\0".as_ptr() as *const _) };

        let shader = RectShaderProgram { id: program, u_color };

        unsafe { gl::UseProgram(0) }

        Ok(shader)
    }

    fn set_color(&self, color: Rgb, alpha: f32) {
        unsafe {
            gl::Uniform4f(
                self.u_color,
                f32::from(color.r) / 255.,
                f32::from(color.g) / 255.,
                f32::from(color.b) / 255.,
                alpha,
            );
        }
    }
}

impl Drop for RectShaderProgram {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteProgram(self.id);
        }
    }
}

fn create_program(vertex: GLuint, fragment: GLuint) -> Result<GLuint, ShaderCreationError> {
    unsafe {
        let program = gl::CreateProgram();
        gl::AttachShader(program, vertex);
        gl::AttachShader(program, fragment);
        gl::LinkProgram(program);

        let mut success: GLint = 0;
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut success);

        if success == i32::from(gl::TRUE) {
            Ok(program)
        } else {
            Err(ShaderCreationError::Link(get_program_info_log(program)))
        }
    }
}

fn create_shader(
    path: &str,
    kind: GLenum,
    source: Option<&'static str>,
) -> Result<GLuint, ShaderCreationError> {
    let from_disk;
    let source = if let Some(src) = source {
        src
    } else {
        from_disk = read_file(path)?;
        &from_disk[..]
    };

    let len: [GLint; 1] = [source.len() as GLint];

    let shader = unsafe {
        let shader = gl::CreateShader(kind);
        gl::ShaderSource(shader, 1, &(source.as_ptr() as *const _), len.as_ptr());
        gl::CompileShader(shader);
        shader
    };

    let mut success: GLint = 0;
    unsafe {
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut success);
    }

    if success == GLint::from(gl::TRUE) {
        Ok(shader)
    } else {
        // Read log
        let log = get_shader_info_log(shader);

        // Cleanup
        unsafe {
            gl::DeleteShader(shader);
        }

        Err(ShaderCreationError::Compile(PathBuf::from(path), log))
    }
}

fn get_program_info_log(program: GLuint) -> String {
    // Get expected log length
    let mut max_length: GLint = 0;
    unsafe {
        gl::GetProgramiv(program, gl::INFO_LOG_LENGTH, &mut max_length);
    }

    // Read the info log
    let mut actual_length: GLint = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(max_length as usize);
    unsafe {
        gl::GetProgramInfoLog(program, max_length, &mut actual_length, buf.as_mut_ptr() as *mut _);
    }

    // Build a string
    unsafe {
        buf.set_len(actual_length as usize);
    }

    // XXX should we expect opengl to return garbage?
    String::from_utf8(buf).unwrap()
}

fn get_shader_info_log(shader: GLuint) -> String {
    // Get expected log length
    let mut max_length: GLint = 0;
    unsafe {
        gl::GetShaderiv(shader, gl::INFO_LOG_LENGTH, &mut max_length);
    }

    // Read the info log
    let mut actual_length: GLint = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(max_length as usize);
    unsafe {
        gl::GetShaderInfoLog(shader, max_length, &mut actual_length, buf.as_mut_ptr() as *mut _);
    }

    // Build a string
    unsafe {
        buf.set_len(actual_length as usize);
    }

    // XXX should we expect opengl to return garbage?
    String::from_utf8(buf).unwrap()
}

fn read_file(path: &str) -> Result<String, io::Error> {
    let mut f = File::open(path)?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;

    Ok(buf)
}

#[derive(Debug)]
pub enum ShaderCreationError {
    /// Error reading file
    Io(io::Error),

    /// Error compiling shader
    Compile(PathBuf, String),

    /// Problem linking
    Link(String),
}

impl ::std::error::Error for ShaderCreationError {
    fn cause(&self) -> Option<&dyn (::std::error::Error)> {
        match *self {
            ShaderCreationError::Io(ref err) => Some(err),
            _ => None,
        }
    }

    fn description(&self) -> &str {
        match *self {
            ShaderCreationError::Io(ref err) => err.description(),
            ShaderCreationError::Compile(ref _path, ref s) => s.as_str(),
            ShaderCreationError::Link(ref s) => s.as_str(),
        }
    }
}

impl ::std::fmt::Display for ShaderCreationError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match *self {
            ShaderCreationError::Io(ref err) => write!(f, "Couldn't read shader: {}", err),
            ShaderCreationError::Compile(ref path, ref log) => {
                write!(f, "Failed compiling shader at {}: {}", path.display(), log)
            },
            ShaderCreationError::Link(ref log) => write!(f, "Failed linking shader: {}", log),
        }
    }
}

impl From<io::Error> for ShaderCreationError {
    fn from(val: io::Error) -> ShaderCreationError {
        ShaderCreationError::Io(val)
    }
}

/// Manages a single texture atlas
///
/// The strategy for filling an atlas looks roughly like this:
///
/// ```text
///                           (width, height)
///   ┌─────┬─────┬─────┬─────┬─────┐
///   │ 10  │     │     │     │     │ <- Empty spaces; can be filled while
///   │     │     │     │     │     │    glyph_height < height - row_baseline
///   ├─────┼─────┼─────┼─────┼─────┤
///   │ 5   │ 6   │ 7   │ 8   │ 9   │
///   │     │     │     │     │     │
///   ├─────┼─────┼─────┼─────┴─────┤ <- Row height is tallest glyph in row; this is
///   │ 1   │ 2   │ 3   │ 4         │    used as the baseline for the following row.
///   │     │     │     │           │ <- Row considered full when next glyph doesn't
///   └─────┴─────┴─────┴───────────┘    fit in the row.
/// (0, 0)  x->
/// ```
#[derive(Debug)]
struct Atlas {
    /// Texture id for this atlas
    id: GLuint,

    /// Width of atlas
    width: i32,

    /// Height of atlas
    height: i32,

    /// Left-most free pixel in a row.
    ///
    /// This is called the extent because it is the upper bound of used pixels
    /// in a row.
    row_extent: i32,

    /// Baseline for glyphs in the current row
    row_baseline: i32,

    /// Tallest glyph in current row
    ///
    /// This is used as the advance when end of row is reached
    row_tallest: i32,
}

/// Error that can happen when inserting a texture to the Atlas
enum AtlasInsertError {
    /// Texture atlas is full
    Full,

    /// The glyph cannot fit within a single texture
    GlyphTooLarge,
}

impl Atlas {
    fn new(size: i32) -> Atlas {
        let mut id: GLuint = 0;
        unsafe {
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGB as i32,
                size,
                size,
                0,
                gl::RGB,
                gl::UNSIGNED_BYTE,
                ptr::null(),
            );

            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);

            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        Atlas { id, width: size, height: size, row_extent: 0, row_baseline: 0, row_tallest: 0 }
    }

    pub fn clear(&mut self) {
        self.row_extent = 0;
        self.row_baseline = 0;
        self.row_tallest = 0;
    }

    /// Insert a RasterizedGlyph into the texture atlas
    pub fn insert(
        &mut self,
        glyph: &RasterizedGlyph,
        active_tex: &mut u32,
    ) -> Result<Glyph, AtlasInsertError> {
        if glyph.width > self.width || glyph.height > self.height {
            return Err(AtlasInsertError::GlyphTooLarge);
        }

        // If there's not enough room in current row, go onto next one
        if !self.room_in_row(glyph) {
            self.advance_row()?;
        }

        // If there's still not room, there's nothing that can be done here.
        if !self.room_in_row(glyph) {
            return Err(AtlasInsertError::Full);
        }

        // There appears to be room; load the glyph.
        Ok(self.insert_inner(glyph, active_tex))
    }

    /// Insert the glyph without checking for room
    ///
    /// Internal function for use once atlas has been checked for space. GL
    /// errors could still occur at this point if we were checking for them;
    /// hence, the Result.
    fn insert_inner(&mut self, glyph: &RasterizedGlyph, active_tex: &mut u32) -> Glyph {
        let offset_y = self.row_baseline;
        let offset_x = self.row_extent;
        let height = glyph.height as i32;
        let width = glyph.width as i32;

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.id);

            // Load data into OpenGL
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                offset_x,
                offset_y,
                width,
                height,
                gl::RGB,
                gl::UNSIGNED_BYTE,
                glyph.buf.as_ptr() as *const _,
            );

            gl::BindTexture(gl::TEXTURE_2D, 0);
            *active_tex = 0;
        }

        // Update Atlas state
        self.row_extent = offset_x + width;
        if height > self.row_tallest {
            self.row_tallest = height;
        }

        // Generate UV coordinates
        let uv_bot = offset_y as f32 / self.height as f32;
        let uv_left = offset_x as f32 / self.width as f32;
        let uv_height = height as f32 / self.height as f32;
        let uv_width = width as f32 / self.width as f32;

        Glyph {
            tex_id: self.id,
            top: glyph.top as f32,
            width: width as f32,
            height: height as f32,
            left: glyph.left as f32,
            uv_bot,
            uv_left,
            uv_width,
            uv_height,
        }
    }

    /// Check if there's room in the current row for given glyph
    fn room_in_row(&self, raw: &RasterizedGlyph) -> bool {
        let next_extent = self.row_extent + raw.width as i32;
        let enough_width = next_extent <= self.width;
        let enough_height = (raw.height as i32) < (self.height - self.row_baseline);

        enough_width && enough_height
    }

    /// Mark current row as finished and prepare to insert into the next row
    fn advance_row(&mut self) -> Result<(), AtlasInsertError> {
        let advance_to = self.row_baseline + self.row_tallest;
        if self.height - advance_to <= 0 {
            return Err(AtlasInsertError::Full);
        }

        self.row_baseline = advance_to;
        self.row_extent = 0;
        self.row_tallest = 0;

        Ok(())
    }
}
