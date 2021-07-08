//! The display subsystem including window management, font rasterization, and
//! GPU drawing.

use std::cmp::min;
use std::convert::TryFrom;
use std::f64;
use std::fmt::{self, Formatter};
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use std::sync::atomic::Ordering;
use std::time::Instant;

use glutin::dpi::{PhysicalPosition, PhysicalSize};
use glutin::event::ModifiersState;
use glutin::event_loop::EventLoop;
#[cfg(not(any(target_os = "macos", windows)))]
use glutin::platform::unix::EventLoopWindowTargetExtUnix;
use glutin::window::CursorIcon;
use log::{debug, info};
use parking_lot::MutexGuard;
use unicode_width::UnicodeWidthChar;
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
use wayland_client::{Display as WaylandDisplay, EventQueue};

use crossfont::{self, Rasterize, Rasterizer};

use alacritty_terminal::ansi::NamedColor;
use alacritty_terminal::event::{EventListener, OnResize};
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::selection::Selection;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{SizeInfo, Term, TermMode, MIN_COLUMNS, MIN_SCREEN_LINES};

use crate::config::font::Font;
use crate::config::window::Dimensions;
#[cfg(not(windows))]
use crate::config::window::StartupMode;
use crate::config::Config;
use crate::display::bell::VisualBell;
use crate::display::color::List;
use crate::display::content::RenderableContent;
use crate::display::cursor::IntoRects;
use crate::display::hint::{HintMatch, HintState};
use crate::display::meter::Meter;
use crate::display::window::Window;
use crate::event::{Mouse, SearchState};
use crate::message_bar::{MessageBuffer, MessageType};
use crate::renderer::rects::{RenderLines, RenderRect};
use crate::renderer::{self, GlyphCache, QuadRenderer};

pub mod content;
pub mod cursor;
pub mod hint;
pub mod window;

mod bell;
mod color;
mod meter;
#[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
mod wayland_theme;

/// Maximum number of linewraps followed outside of the viewport during search highlighting.
pub const MAX_SEARCH_LINES: usize = 100;

/// Label for the forward terminal search bar.
const FORWARD_SEARCH_LABEL: &str = "Search: ";

/// Label for the backward terminal search bar.
const BACKWARD_SEARCH_LABEL: &str = "Backward Search: ";

#[derive(Debug)]
pub enum Error {
    /// Error with window management.
    Window(window::Error),

    /// Error dealing with fonts.
    Font(crossfont::Error),

    /// Error in renderer.
    Render(renderer::Error),

    /// Error during buffer swap.
    Context(glutin::ContextError),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Window(err) => err.source(),
            Error::Font(err) => err.source(),
            Error::Render(err) => err.source(),
            Error::Context(err) => err.source(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Window(err) => err.fmt(f),
            Error::Font(err) => err.fmt(f),
            Error::Render(err) => err.fmt(f),
            Error::Context(err) => err.fmt(f),
        }
    }
}

impl From<window::Error> for Error {
    fn from(val: window::Error) -> Self {
        Error::Window(val)
    }
}

impl From<crossfont::Error> for Error {
    fn from(val: crossfont::Error) -> Self {
        Error::Font(val)
    }
}

impl From<renderer::Error> for Error {
    fn from(val: renderer::Error) -> Self {
        Error::Render(val)
    }
}

impl From<glutin::ContextError> for Error {
    fn from(val: glutin::ContextError) -> Self {
        Error::Context(val)
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct DisplayUpdate {
    pub dirty: bool,

    dimensions: Option<PhysicalSize<u32>>,
    cursor_dirty: bool,
    font: Option<Font>,
}

impl DisplayUpdate {
    pub fn dimensions(&self) -> Option<PhysicalSize<u32>> {
        self.dimensions
    }

    pub fn font(&self) -> Option<&Font> {
        self.font.as_ref()
    }

    pub fn cursor_dirty(&self) -> bool {
        self.cursor_dirty
    }

    pub fn set_dimensions(&mut self, dimensions: PhysicalSize<u32>) {
        self.dimensions = Some(dimensions);
        self.dirty = true;
    }

    pub fn set_font(&mut self, font: Font) {
        self.font = Some(font);
        self.dirty = true;
    }

    pub fn set_cursor_dirty(&mut self) {
        self.cursor_dirty = true;
        self.dirty = true;
    }
}

/// The display wraps a window, font rasterizer, and GPU renderer.
pub struct Display {
    pub size_info: SizeInfo,
    pub window: Window,

    /// Hint highlighted by the mouse.
    pub highlighted_hint: Option<HintMatch>,

    /// Hint highlighted by the vi mode cursor.
    pub vi_highlighted_hint: Option<HintMatch>,

    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    pub wayland_event_queue: Option<EventQueue>,

    #[cfg(not(any(target_os = "macos", windows)))]
    pub is_x11: bool,

    /// UI cursor visibility for blinking.
    pub cursor_hidden: bool,

    pub visual_bell: VisualBell,

    /// Mapped RGB values for each terminal color.
    pub colors: List,

    /// State of the keyboard hints.
    pub hint_state: HintState,

    renderer: QuadRenderer,
    glyph_cache: GlyphCache,
    meter: Meter,
}

impl Display {
    pub fn new<E>(config: &Config, event_loop: &EventLoop<E>) -> Result<Display, Error> {
        #[cfg(any(not(feature = "x11"), target_os = "macos", windows))]
        let is_x11 = false;
        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        let is_x11 = event_loop.is_x11();

        // Guess DPR based on first monitor. On Wayland the initial frame always renders at a DPR
        // of 1.
        let estimated_dpr = if cfg!(any(target_os = "macos", windows)) || is_x11 {
            event_loop.available_monitors().next().map(|m| m.scale_factor()).unwrap_or(1.)
        } else {
            1.
        };

        // Guess the target window dimensions.
        let metrics = GlyphCache::static_metrics(config.ui_config.font.clone(), estimated_dpr)?;
        let (cell_width, cell_height) = compute_cell_size(config, &metrics);

        // Guess the target window size if the user has specified the number of lines/columns.
        let dimensions = config.ui_config.window.dimensions();
        let estimated_size = dimensions.map(|dimensions| {
            window_size(config, dimensions, cell_width, cell_height, estimated_dpr)
        });

        debug!("Estimated DPR: {}", estimated_dpr);
        debug!("Estimated window size: {:?}", estimated_size);
        debug!("Estimated cell size: {} x {}", cell_width, cell_height);

        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        let mut wayland_event_queue = None;

        // Initialize Wayland event queue, to handle Wayland callbacks.
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        if let Some(display) = event_loop.wayland_display() {
            let display = unsafe { WaylandDisplay::from_external_display(display as _) };
            wayland_event_queue = Some(display.create_event_queue());
        }

        // Spawn the Alacritty window.
        let mut window = Window::new(
            event_loop,
            config,
            estimated_size,
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            wayland_event_queue.as_ref(),
        )?;

        info!("Device pixel ratio: {}", window.dpr);

        // Create renderer.
        let mut renderer = QuadRenderer::new()?;

        let (glyph_cache, cell_width, cell_height) =
            Self::new_glyph_cache(window.dpr, &mut renderer, config)?;

        if let Some(dimensions) = dimensions {
            if (estimated_dpr - window.dpr).abs() < f64::EPSILON {
                info!("Estimated DPR correctly, skipping resize");
            } else {
                // Resize the window again if the DPR was not estimated correctly.
                let size = window_size(config, dimensions, cell_width, cell_height, window.dpr);
                window.set_inner_size(size);
            }
        }

        let padding = config.ui_config.window.padding(window.dpr);
        let viewport_size = window.inner_size();

        // Create new size with at least one column and row.
        let size_info = SizeInfo::new(
            viewport_size.width as f32,
            viewport_size.height as f32,
            cell_width,
            cell_height,
            padding.0,
            padding.1,
            config.ui_config.window.dynamic_padding && dimensions.is_none(),
        );

        info!("Cell size: {} x {}", cell_width, cell_height);
        info!("Padding: {} x {}", size_info.padding_x(), size_info.padding_y());
        info!("Width: {}, Height: {}", size_info.width(), size_info.height());

        // Update OpenGL projection.
        renderer.resize(&size_info);

        // Clear screen.
        let background_color = config.ui_config.colors.primary.background;
        renderer.with_api(&config.ui_config, &size_info, |api| {
            api.clear(background_color);
        });

        // Set subpixel anti-aliasing.
        #[cfg(target_os = "macos")]
        crossfont::set_font_smoothing(config.ui_config.font.use_thin_strokes);

        // Disable shadows for transparent windows on macOS.
        #[cfg(target_os = "macos")]
        window.set_has_shadow(config.ui_config.background_opacity() >= 1.0);

        // On Wayland we can safely ignore this call, since the window isn't visible until you
        // actually draw something into it and commit those changes.
        #[cfg(not(any(target_os = "macos", windows)))]
        if is_x11 {
            window.swap_buffers();
            renderer.with_api(&config.ui_config, &size_info, |api| {
                api.finish();
            });
        }

        window.set_visible(true);

        // Set window position.
        //
        // TODO: replace `set_position` with `with_position` once available.
        // Upstream issue: https://github.com/rust-windowing/winit/issues/806.
        if let Some(position) = config.ui_config.window.position {
            window.set_outer_position(PhysicalPosition::from((position.x, position.y)));
        }

        #[allow(clippy::single_match)]
        #[cfg(not(windows))]
        match config.ui_config.window.startup_mode {
            #[cfg(target_os = "macos")]
            StartupMode::SimpleFullscreen => window.set_simple_fullscreen(true),
            #[cfg(not(target_os = "macos"))]
            StartupMode::Maximized if is_x11 => window.set_maximized(true),
            _ => (),
        }

        let hint_state = HintState::new(config.ui_config.hints.alphabet());

        Ok(Self {
            window,
            renderer,
            glyph_cache,
            hint_state,
            meter: Meter::new(),
            size_info,
            highlighted_hint: None,
            vi_highlighted_hint: None,
            #[cfg(not(any(target_os = "macos", windows)))]
            is_x11,
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            wayland_event_queue,
            cursor_hidden: false,
            visual_bell: VisualBell::from(&config.ui_config.bell),
            colors: List::from(&config.ui_config.colors),
        })
    }

    fn new_glyph_cache(
        dpr: f64,
        renderer: &mut QuadRenderer,
        config: &Config,
    ) -> Result<(GlyphCache, f32, f32), Error> {
        let font = config.ui_config.font.clone();
        let rasterizer = Rasterizer::new(dpr as f32, config.ui_config.font.use_thin_strokes)?;

        // Initialize glyph cache.
        let glyph_cache = {
            info!("Initializing glyph cache...");
            let init_start = Instant::now();

            let cache =
                renderer.with_loader(|mut api| GlyphCache::new(rasterizer, &font, &mut api))?;

            let stop = init_start.elapsed();
            let stop_f = stop.as_secs() as f64 + f64::from(stop.subsec_nanos()) / 1_000_000_000f64;
            info!("... finished initializing glyph cache in {}s", stop_f);

            cache
        };

        // Need font metrics to resize the window properly. This suggests to me the
        // font metrics should be computed before creating the window in the first
        // place so that a resize is not needed.
        let (cw, ch) = compute_cell_size(config, &glyph_cache.font_metrics());

        Ok((glyph_cache, cw, ch))
    }

    /// Update font size and cell dimensions.
    ///
    /// This will return a tuple of the cell width and height.
    fn update_glyph_cache(&mut self, config: &Config, font: &Font) -> (f32, f32) {
        let cache = &mut self.glyph_cache;
        let dpr = self.window.dpr;

        self.renderer.with_loader(|mut api| {
            let _ = cache.update_font_size(font, dpr, &mut api);
        });

        // Compute new cell sizes.
        compute_cell_size(config, &self.glyph_cache.font_metrics())
    }

    /// Clear glyph cache.
    fn clear_glyph_cache(&mut self) {
        let cache = &mut self.glyph_cache;
        self.renderer.with_loader(|mut api| {
            cache.clear_glyph_cache(&mut api);
        });
    }

    /// Process update events.
    pub fn handle_update<T>(
        &mut self,
        terminal: &mut Term<T>,
        pty_resize_handle: &mut dyn OnResize,
        message_buffer: &MessageBuffer,
        search_active: bool,
        config: &Config,
        update_pending: DisplayUpdate,
    ) where
        T: EventListener,
    {
        let (mut cell_width, mut cell_height) =
            (self.size_info.cell_width(), self.size_info.cell_height());

        // Update font size and cell dimensions.
        if let Some(font) = update_pending.font() {
            let cell_dimensions = self.update_glyph_cache(config, font);
            cell_width = cell_dimensions.0;
            cell_height = cell_dimensions.1;

            info!("Cell size: {} x {}", cell_width, cell_height);
        } else if update_pending.cursor_dirty() {
            self.clear_glyph_cache();
        }

        let (mut width, mut height) = (self.size_info.width(), self.size_info.height());
        if let Some(dimensions) = update_pending.dimensions() {
            width = dimensions.width as f32;
            height = dimensions.height as f32;
        }

        let padding = config.ui_config.window.padding(self.window.dpr);

        self.size_info = SizeInfo::new(
            width,
            height,
            cell_width,
            cell_height,
            padding.0,
            padding.1,
            config.ui_config.window.dynamic_padding,
        );

        // Update number of column/lines in the viewport.
        let message_bar_lines =
            message_buffer.message().map(|m| m.text(&self.size_info).len()).unwrap_or(0);
        let search_lines = if search_active { 1 } else { 0 };
        self.size_info.reserve_lines(message_bar_lines + search_lines);

        // Resize PTY.
        pty_resize_handle.on_resize(&self.size_info);

        // Resize terminal.
        terminal.resize(self.size_info);

        // Resize renderer.
        let physical =
            PhysicalSize::new(self.size_info.width() as u32, self.size_info.height() as u32);
        self.window.resize(physical);
        self.renderer.resize(&self.size_info);

        info!("Padding: {} x {}", self.size_info.padding_x(), self.size_info.padding_y());
        info!("Width: {}, Height: {}", self.size_info.width(), self.size_info.height());
    }

    /// Draw the screen.
    ///
    /// A reference to Term whose state is being drawn must be provided.
    ///
    /// This call may block if vsync is enabled.
    pub fn draw<T: EventListener>(
        &mut self,
        mut terminal: MutexGuard<'_, Term<T>>,
        message_buffer: &MessageBuffer,
        config: &Config,
        search_state: &SearchState,
    ) {
        // Collect renderable content before the terminal is dropped.
        let mut content = RenderableContent::new(config, self, &terminal, search_state);
        let mut grid_cells = Vec::new();
        for cell in &mut content {
            grid_cells.push(cell);
        }
        let background_color = content.color(NamedColor::Background as usize);
        let display_offset = content.display_offset();
        let cursor = content.cursor();

        let cursor_point = terminal.grid().cursor.point;
        let total_lines = terminal.grid().total_lines();
        let metrics = self.glyph_cache.font_metrics();
        let size_info = self.size_info;

        let vi_mode = terminal.mode().contains(TermMode::VI);
        let vi_mode_cursor = if vi_mode { Some(terminal.vi_mode_cursor) } else { None };

        let graphics_queues = terminal.graphics_take_queues();

        // Drop terminal as early as possible to free lock.
        drop(terminal);

        self.renderer.with_api(&config.ui_config, &size_info, |api| {
            api.clear(background_color);
        });

        if let Some(graphics_queues) = graphics_queues {
            self.renderer.graphics_run_updates(graphics_queues, &size_info);
        }

        let mut lines = RenderLines::new();
        let mut graphics_list = renderer::graphics::RenderList::default();

        // Draw grid.
        {
            let _sampler = self.meter.sampler();

            let glyph_cache = &mut self.glyph_cache;
            let highlighted_hint = &self.highlighted_hint;
            let vi_highlighted_hint = &self.vi_highlighted_hint;
            self.renderer.with_api(&config.ui_config, &size_info, |mut api| {
                // Iterate over all non-empty cells in the grid.
                for mut cell in grid_cells {
                    // Underline hints hovered by mouse or vi mode cursor.
                    let point = viewport_to_point(display_offset, cell.point);
                    if highlighted_hint.as_ref().map_or(false, |h| h.bounds.contains(&point))
                        || vi_highlighted_hint.as_ref().map_or(false, |h| h.bounds.contains(&point))
                    {
                        cell.flags.insert(Flags::UNDERLINE);
                    }

                    // Update underline/strikeout.
                    lines.update(&cell);

                    // Track any graphic present in the cell.
                    graphics_list.update(&cell);

                    // Draw the cell.
                    api.render_cell(cell, glyph_cache);
                }
            });
        }

        self.renderer.graphics_draw(graphics_list, &size_info);

        let mut rects = lines.rects(&metrics, &size_info);

        if let Some(vi_mode_cursor) = vi_mode_cursor {
            // Indicate vi mode by showing the cursor's position in the top right corner.
            let vi_point = vi_mode_cursor.point;
            let line = (-vi_point.line.0 + size_info.bottommost_line().0) as usize;
            self.draw_line_indicator(config, &size_info, total_lines, Some(vi_point), line);
        } else if search_state.regex().is_some() {
            // Show current display offset in vi-less search to indicate match position.
            self.draw_line_indicator(config, &size_info, total_lines, None, display_offset);
        }

        // Push the cursor rects for rendering.
        if let Some(cursor) = cursor {
            for rect in cursor.rects(&size_info, config.cursor.thickness()) {
                rects.push(rect);
            }
        }

        // Push visual bell after url/underline/strikeout rects.
        let visual_bell_intensity = self.visual_bell.intensity();
        if visual_bell_intensity != 0. {
            let visual_bell_rect = RenderRect::new(
                0.,
                0.,
                size_info.width(),
                size_info.height(),
                config.ui_config.bell.color,
                visual_bell_intensity as f32,
            );
            rects.push(visual_bell_rect);
        }

        if let Some(message) = message_buffer.message() {
            let search_offset = if search_state.regex().is_some() { 1 } else { 0 };
            let text = message.text(&size_info);

            // Create a new rectangle for the background.
            let start_line = size_info.screen_lines() + search_offset;
            let y = size_info.cell_height().mul_add(start_line as f32, size_info.padding_y());

            let bg = match message.ty() {
                MessageType::Error => config.ui_config.colors.normal.red,
                MessageType::Warning => config.ui_config.colors.normal.yellow,
            };

            let message_bar_rect =
                RenderRect::new(0., y, size_info.width(), size_info.height() - y, bg, 1.);

            // Push message_bar in the end, so it'll be above all other content.
            rects.push(message_bar_rect);

            // Draw rectangles.
            self.renderer.draw_rects(&size_info, rects);

            // Relay messages to the user.
            let glyph_cache = &mut self.glyph_cache;
            let fg = config.ui_config.colors.primary.background;
            for (i, message_text) in text.iter().enumerate() {
                let point = Point::new(start_line + i, Column(0));
                self.renderer.with_api(&config.ui_config, &size_info, |mut api| {
                    api.render_string(glyph_cache, point, fg, bg, message_text);
                });
            }
        } else {
            // Draw rectangles.
            self.renderer.draw_rects(&size_info, rects);
        }

        self.draw_render_timer(config, &size_info);

        // Handle search and IME positioning.
        let ime_position = match search_state.regex() {
            Some(regex) => {
                let search_label = match search_state.direction() {
                    Direction::Right => FORWARD_SEARCH_LABEL,
                    Direction::Left => BACKWARD_SEARCH_LABEL,
                };

                let search_text = Self::format_search(&size_info, regex, search_label);

                // Render the search bar.
                self.draw_search(config, &size_info, &search_text);

                // Compute IME position.
                let line = Line(size_info.screen_lines() as i32 + 1);
                Point::new(line, Column(search_text.chars().count() - 1))
            },
            None => cursor_point,
        };

        // Update IME position.
        self.window.update_ime_position(ime_position, &self.size_info);

        // Frame event should be requested before swaping buffers, since it requires surface
        // `commit`, which is done by swap buffers under the hood.
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        self.request_frame(&self.window);

        self.window.swap_buffers();

        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        if self.is_x11 {
            // On X11 `swap_buffers` does not block for vsync. However the next OpenGl command
            // will block to synchronize (this is `glClear` in Alacritty), which causes a
            // permanent one frame delay.
            self.renderer.with_api(&config.ui_config, &size_info, |api| {
                api.finish();
            });
        }
    }

    /// Update to a new configuration.
    pub fn update_config(&mut self, config: &Config) {
        self.visual_bell.update_config(&config.ui_config.bell);
        self.colors = List::from(&config.ui_config.colors);
    }

    /// Update the mouse/vi mode cursor hint highlighting.
    ///
    /// This will return whether the highlighted hints changed.
    pub fn update_highlighted_hints<T>(
        &mut self,
        term: &Term<T>,
        config: &Config,
        mouse: &Mouse,
        modifiers: ModifiersState,
    ) -> bool {
        // Update vi mode cursor hint.
        let vi_highlighted_hint = if term.mode().contains(TermMode::VI) {
            let mods = ModifiersState::all();
            let point = term.vi_mode_cursor.point;
            hint::highlighted_at(term, config, point, mods)
        } else {
            None
        };
        let mut dirty = vi_highlighted_hint != self.vi_highlighted_hint;
        self.vi_highlighted_hint = vi_highlighted_hint;

        // Abort if mouse highlighting conditions are not met.
        if !mouse.inside_text_area || !term.selection.as_ref().map_or(true, Selection::is_empty) {
            dirty |= self.highlighted_hint.is_some();
            self.highlighted_hint = None;
            return dirty;
        }

        // Find highlighted hint at mouse position.
        let point = mouse.point(&self.size_info, term.grid().display_offset());
        let highlighted_hint = hint::highlighted_at(term, config, point, modifiers);

        // Update cursor shape.
        if highlighted_hint.is_some() {
            self.window.set_mouse_cursor(CursorIcon::Hand);
        } else if self.highlighted_hint.is_some() {
            if term.mode().intersects(TermMode::MOUSE_MODE) && !term.mode().contains(TermMode::VI) {
                self.window.set_mouse_cursor(CursorIcon::Default);
            } else {
                self.window.set_mouse_cursor(CursorIcon::Text);
            }
        }

        dirty |= self.highlighted_hint != highlighted_hint;
        self.highlighted_hint = highlighted_hint;

        dirty
    }

    /// Format search regex to account for the cursor and fullwidth characters.
    fn format_search(size_info: &SizeInfo, search_regex: &str, search_label: &str) -> String {
        // Add spacers for wide chars.
        let mut formatted_regex = String::with_capacity(search_regex.len());
        for c in search_regex.chars() {
            formatted_regex.push(c);
            if c.width() == Some(2) {
                formatted_regex.push(' ');
            }
        }

        // Add cursor to show whitespace.
        formatted_regex.push('_');

        // Truncate beginning of the search regex if it exceeds the viewport width.
        let num_cols = size_info.columns();
        let label_len = search_label.chars().count();
        let regex_len = formatted_regex.chars().count();
        let truncate_len = min((regex_len + label_len).saturating_sub(num_cols), regex_len);
        let index = formatted_regex.char_indices().nth(truncate_len).map(|(i, _c)| i).unwrap_or(0);
        let truncated_regex = &formatted_regex[index..];

        // Add search label to the beginning of the search regex.
        let mut bar_text = format!("{}{}", search_label, truncated_regex);

        // Make sure the label alone doesn't exceed the viewport width.
        bar_text.truncate(num_cols);

        bar_text
    }

    /// Draw current search regex.
    fn draw_search(&mut self, config: &Config, size_info: &SizeInfo, text: &str) {
        let glyph_cache = &mut self.glyph_cache;
        let num_cols = size_info.columns();

        // Assure text length is at least num_cols.
        let text = format!("{:<1$}", text, num_cols);

        let point = Point::new(size_info.screen_lines(), Column(0));
        let fg = config.ui_config.colors.search_bar_foreground();
        let bg = config.ui_config.colors.search_bar_background();

        self.renderer.with_api(&config.ui_config, size_info, |mut api| {
            api.render_string(glyph_cache, point, fg, bg, &text);
        });
    }

    /// Draw render timer.
    fn draw_render_timer(&mut self, config: &Config, size_info: &SizeInfo) {
        if !config.ui_config.debug.render_timer {
            return;
        }

        let glyph_cache = &mut self.glyph_cache;

        let timing = format!("{:.3} usec", self.meter.average());
        let point = Point::new(size_info.screen_lines().saturating_sub(2), Column(0));
        let fg = config.ui_config.colors.primary.background;
        let bg = config.ui_config.colors.normal.red;

        self.renderer.with_api(&config.ui_config, size_info, |mut api| {
            api.render_string(glyph_cache, point, fg, bg, &timing);
        });
    }

    /// Draw an indicator for the position of a line in history.
    fn draw_line_indicator(
        &mut self,
        config: &Config,
        size_info: &SizeInfo,
        total_lines: usize,
        vi_mode_point: Option<Point>,
        line: usize,
    ) {
        let text = format!("[{}/{}]", line, total_lines - 1);
        let column = Column(size_info.columns().saturating_sub(text.len()));
        let colors = &config.ui_config.colors;
        let fg = colors.line_indicator.foreground.unwrap_or(colors.primary.background);
        let bg = colors.line_indicator.background.unwrap_or(colors.primary.foreground);

        // Do not render anything if it would obscure the vi mode cursor.
        if vi_mode_point.map_or(true, |point| point.line != 0 || point.column < column) {
            let glyph_cache = &mut self.glyph_cache;
            self.renderer.with_api(&config.ui_config, size_info, |mut api| {
                api.render_string(glyph_cache, Point::new(0, column), fg, bg, &text);
            });
        }
    }

    /// Requst a new frame for a window on Wayland.
    #[inline]
    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    fn request_frame(&self, window: &Window) {
        let surface = match window.wayland_surface() {
            Some(surface) => surface,
            None => return,
        };

        let should_draw = self.window.should_draw.clone();

        // Mark that window was drawn.
        should_draw.store(false, Ordering::Relaxed);

        // Request a new frame.
        surface.frame().quick_assign(move |_, _, _| {
            should_draw.store(true, Ordering::Relaxed);
        });
    }
}

/// Convert a terminal point to a viewport relative point.
pub fn point_to_viewport(display_offset: usize, point: Point) -> Option<Point<usize>> {
    let viewport_line = point.line.0 + display_offset as i32;
    usize::try_from(viewport_line).ok().map(|line| Point::new(line, point.column))
}

/// Convert a viewport relative point to a terminal point.
pub fn viewport_to_point(display_offset: usize, point: Point<usize>) -> Point {
    let line = Line(point.line as i32) - display_offset;
    Point::new(line, point.column)
}

/// Calculate the cell dimensions based on font metrics.
///
/// This will return a tuple of the cell width and height.
#[inline]
fn compute_cell_size(config: &Config, metrics: &crossfont::Metrics) -> (f32, f32) {
    let offset_x = f64::from(config.ui_config.font.offset.x);
    let offset_y = f64::from(config.ui_config.font.offset.y);
    (
        (metrics.average_advance + offset_x).floor().max(1.) as f32,
        (metrics.line_height + offset_y).floor().max(1.) as f32,
    )
}

/// Calculate the size of the window given padding, terminal dimensions and cell size.
fn window_size(
    config: &Config,
    dimensions: Dimensions,
    cell_width: f32,
    cell_height: f32,
    dpr: f64,
) -> PhysicalSize<u32> {
    let padding = config.ui_config.window.padding(dpr);

    let grid_width = cell_width * dimensions.columns.0.max(MIN_COLUMNS) as f32;
    let grid_height = cell_height * dimensions.lines.max(MIN_SCREEN_LINES) as f32;

    let width = (padding.0).mul_add(2., grid_width).floor();
    let height = (padding.1).mul_add(2., grid_height).floor();

    PhysicalSize::new(width as u32, height as u32)
}
