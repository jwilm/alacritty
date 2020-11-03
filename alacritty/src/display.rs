//! The display subsystem including window management, font rasterization, and
//! GPU drawing.

use std::cmp::min;
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

#[cfg(target_os = "macos")]
use crossfont::set_font_smoothing;
use crossfont::{self, Rasterize, Rasterizer};

use alacritty_terminal::event::{EventListener, OnResize};
use alacritty_terminal::index::{Column, Direction, Point};
use alacritty_terminal::selection::Selection;
use alacritty_terminal::term::{RenderableCell, SizeInfo, Term, TermMode};
use alacritty_terminal::term::{MIN_COLS, MIN_SCREEN_LINES};

use crate::config::font::Font;
use crate::config::window::Dimensions;
#[cfg(not(windows))]
use crate::config::window::StartupMode;
use crate::config::Config;
use crate::event::{Mouse, SearchState};
use crate::message_bar::{MessageBuffer, MessageType};
use crate::meter::Meter;
use crate::renderer::rects::{RenderLines, RenderRect};
use crate::renderer::{self, GlyphCache, RenderContext, Renderer};
use crate::url::{Url, Urls};
use crate::window::{self, Window};

const FORWARD_SEARCH_LABEL: &str = "Search: ";
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
    ContextError(glutin::ContextError),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Window(err) => err.source(),
            Error::Font(err) => err.source(),
            Error::Render(err) => err.source(),
            Error::ContextError(err) => err.source(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::Window(err) => err.fmt(f),
            Error::Font(err) => err.fmt(f),
            Error::Render(err) => err.fmt(f),
            Error::ContextError(err) => err.fmt(f),
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
        Error::ContextError(val)
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
    pub urls: Urls,

    /// Currently highlighted URL.
    pub highlighted_url: Option<Url>,

    #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
    pub wayland_event_queue: Option<EventQueue>,

    #[cfg(feature = "dump-raw-render-timings")]
    timing_dump_file: std::fs::File,

    #[cfg(not(any(target_os = "macos", windows)))]
    pub is_x11: bool,

    renderer: Renderer,
    glyph_cache: GlyphCache,
    meter: Meter,
}

impl Display {
    pub fn new<E>(config: &Config, event_loop: &EventLoop<E>) -> Result<Display, Error> {
        // Guess DPR based on first monitor.
        let estimated_dpr =
            event_loop.available_monitors().next().map(|m| m.scale_factor()).unwrap_or(1.);

        // Guess the target window dimensions.
        let metrics = GlyphCache::static_metrics(config.ui_config.font.clone(), estimated_dpr)?;
        let (cell_width, cell_height) = GlyphCache::compute_cell_size(config, &metrics);

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
            &config,
            estimated_size,
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            wayland_event_queue.as_ref(),
        )?;

        info!("Device pixel ratio: {}", window.dpr);

        // Create renderer.
        let mut renderer = Renderer::new()?;

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
        let background_color = config.colors.primary.background;
        renderer.clear(background_color, config.ui_config.background_opacity());

        // Set subpixel anti-aliasing.
        #[cfg(target_os = "macos")]
        set_font_smoothing(config.ui_config.font.use_thin_strokes());

        #[cfg(all(feature = "x11", not(any(target_os = "macos", windows))))]
        let is_x11 = event_loop.is_x11();
        #[cfg(not(any(feature = "x11", target_os = "macos", windows)))]
        let is_x11 = false;

        // On Wayland we can safely ignore this call, since the window isn't visible until you
        // actually draw something into it and commit those changes.
        #[cfg(not(any(target_os = "macos", windows)))]
        if is_x11 {
            window.swap_buffers();
            renderer.finish();
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

        Ok(Self {
            window,
            renderer,
            glyph_cache,
            meter: Meter::new(),
            size_info,
            urls: Urls::new(),
            highlighted_url: None,
            #[cfg(not(any(target_os = "macos", windows)))]
            is_x11,
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            wayland_event_queue,
            #[cfg(feature = "dump-raw-render-timings")]
            timing_dump_file: std::fs::File::create("timing.dump").unwrap(),
        })
    }

    fn new_glyph_cache(
        dpr: f64,
        renderer: &mut Renderer,
        config: &Config,
    ) -> Result<(GlyphCache, f32, f32), Error> {
        let font = config.ui_config.font.clone();
        let rasterizer = Rasterizer::new(dpr as f32, config.ui_config.font.use_thin_strokes())?;

        // Initialize glyph cache.
        let glyph_cache = {
            info!("Initializing glyph cache...");
            let init_start = Instant::now();

            let cache = renderer
                .with_loader(|mut api| GlyphCache::new(rasterizer, config, &font, &mut api))?;

            let stop = init_start.elapsed();
            let stop_f = stop.as_secs() as f64 + f64::from(stop.subsec_nanos()) / 1_000_000_000f64;
            info!("... finished initializing glyph cache in {}s", stop_f);

            cache
        };

        // Need font metrics to resize the window properly. This suggests to me the
        // font metrics should be computed before creating the window in the first
        // place so that a resize is not needed.
        let (cw, ch) = GlyphCache::compute_cell_size(config, &glyph_cache.font_metrics());

        Ok((glyph_cache, cw, ch))
    }

    /// Update font size and cell dimensions.
    ///
    /// This will return a tuple of the cell width and height.
    fn update_glyph_cache(&mut self, config: &Config, font: &Font) -> (f32, f32) {
        let cache = &mut self.glyph_cache;
        let dpr = self.window.dpr;

        self.renderer.with_loader(|mut api| {
            let _ = cache.update_font_size(config, font, dpr, &mut api);
        });

        // Compute new cell sizes.
        GlyphCache::compute_cell_size(config, &self.glyph_cache.font_metrics())
    }

    /// Clear glyph cache.
    fn clear_glyph_cache(&mut self, config: &Config) {
        let cache = &mut self.glyph_cache;
        self.renderer.with_loader(|mut api| {
            cache.clear_glyph_cache(config, &mut api);
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
            self.clear_glyph_cache(config);
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
    pub fn draw<T>(
        &mut self,
        terminal: MutexGuard<'_, Term<T>>,
        message_buffer: &MessageBuffer,
        config: &Config,
        mouse: &Mouse,
        mods: ModifiersState,
        search_state: &SearchState,
    ) {
        let grid_cells: Vec<RenderableCell> = terminal.renderable_cells(config).collect();
        let visual_bell_intensity = terminal.visual_bell.intensity();
        let background_color = terminal.background_color();
        let cursor_point = terminal.grid().cursor.point;
        let metrics = self.glyph_cache.font_metrics();
        let glyph_cache = &mut self.glyph_cache;
        let size_info = self.size_info;

        let selection = !terminal.selection.as_ref().map(Selection::is_empty).unwrap_or(true);
        let mouse_mode = terminal.mode().intersects(TermMode::MOUSE_MODE)
            && !terminal.mode().contains(TermMode::VI);

        let vi_mode_cursor = if terminal.mode().contains(TermMode::VI) {
            Some(terminal.vi_mode_cursor)
        } else {
            None
        };

        // Drop terminal as early as possible to free lock.
        drop(terminal);

        #[cfg(feature = "dump-raw-render-timings")]
        let start = Instant::now();

        self.renderer.clear(background_color, config.ui_config.background_opacity());

        let mut render_context = self.renderer.begin(&config.ui_config, config.cursor, &size_info);

        let mut lines = RenderLines::new();
        let mut urls = Urls::new();

        // Draw grid.
        {
            let _sampler = self.meter.sampler();

            // Iterate over all non-empty cells in the grid.
            for cell in grid_cells {
                // Update URL underlines.
                urls.update(size_info.cols(), cell);

                // Update underline/strikeout.
                lines.update(cell);

                // Draw the cell.
                render_context.update_cell(cell, glyph_cache);
            }
        }

        if let Some(message) = message_buffer.message() {
            let search_offset = if search_state.regex().is_some() { 1 } else { 0 };
            let text = message.text(&size_info);

            let start_line = size_info.screen_lines() + search_offset;

            let color = match message.ty() {
                MessageType::Error => config.colors.normal().red,
                MessageType::Warning => config.colors.normal().yellow,
            };

            // Relay messages to the user.
            let fg = config.colors.primary.background;
            for (i, message_text) in text.iter().enumerate() {
                render_context.render_string(
                    glyph_cache,
                    start_line + i,
                    &message_text,
                    fg,
                    Some(color),
                );
            }
        }

        Self::draw_render_timer(
            &mut self.glyph_cache,
            &mut render_context,
            config,
            &size_info,
            &self.meter,
        );

        // Handle search and IME positioning.
        let ime_position = match search_state.regex() {
            Some(regex) => {
                let search_label = match search_state.direction() {
                    Direction::Right => FORWARD_SEARCH_LABEL,
                    Direction::Left => BACKWARD_SEARCH_LABEL,
                };

                let search_text = Self::format_search(&size_info, regex, search_label);

                // Render the search bar.
                Self::draw_search(
                    &mut self.glyph_cache,
                    &mut render_context,
                    config,
                    &size_info,
                    &search_text,
                );

                // Compute IME position.
                Point::new(size_info.screen_lines() + 1, Column(search_text.chars().count() - 1))
            },
            None => cursor_point,
        };

        // Update IME position.
        self.window.update_ime_position(ime_position, &self.size_info);

        render_context.draw_text();

        let mut rects = lines.rects(&metrics, &size_info);

        // Update visible URLs.
        self.urls = urls;
        if let Some(url) = self.urls.highlighted(config, mouse, mods, mouse_mode, selection) {
            rects.append(&mut url.rects(&metrics, &size_info));

            self.window.set_mouse_cursor(CursorIcon::Hand);

            self.highlighted_url = Some(url);
        } else if self.highlighted_url.is_some() {
            self.highlighted_url = None;

            if mouse_mode {
                self.window.set_mouse_cursor(CursorIcon::Default);
            } else {
                self.window.set_mouse_cursor(CursorIcon::Text);
            }
        }

        // Highlight URLs at the vi mode cursor position.
        if let Some(vi_mode_cursor) = vi_mode_cursor {
            if let Some(url) = self.urls.find_at(vi_mode_cursor.point) {
                rects.append(&mut url.rects(&metrics, &size_info));
            }
        }

        // Push visual bell after url/underline/strikeout rects.
        if visual_bell_intensity != 0. {
            let visual_bell_rect = RenderRect::new(
                0.,
                0.,
                size_info.width(),
                size_info.height(),
                config.bell().color,
                visual_bell_intensity as f32,
            );
            rects.push(visual_bell_rect);
        }

        // Draw rectangles.
        render_context.draw_rects(rects);

        drop(render_context);

        #[cfg(feature = "dump-raw-render-timings")]
        {
            self.renderer.finish();

            let dt = (Instant::now() - start).as_micros() as u32;
            std::io::Write::write(&mut self.timing_dump_file, &dt.to_ne_bytes()).unwrap();
        }

        // Frame event should be requested before swaping buffers, since it requires surface
        // `commit`, which is done by swap buffers under the hood.
        #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
        self.request_frame(&self.window);

        #[cfg(all(
            feature = "x11",
            not(any(target_os = "macos", windows)),
            not(feature = "dump-raw-render-timings")
        ))]
        if self.is_x11 {
            // On X11 `swap_buffers` does not block for vsync. However the next OpenGl command
            // will block to synchronize (this is `glClear` in Alacritty), which causes a
            // permanent one frame delay.
            self.renderer.finish();
        }

        self.window.swap_buffers();
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
        let num_cols = size_info.cols().0;
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
    fn draw_search(
        glyph_cache: &mut GlyphCache,
        render_context: &mut RenderContext<'_>,
        config: &Config,
        size_info: &SizeInfo,
        text: &str,
    ) {
        let num_cols = size_info.cols().0;

        // Assure text length is at least num_cols.
        let text = format!("{:<1$}", text, num_cols);

        let fg = config.colors.search_bar_foreground();
        let bg = config.colors.search_bar_background();
        render_context.render_string(glyph_cache, size_info.screen_lines(), &text, fg, Some(bg));
    }

    /// Draw render timer.
    fn draw_render_timer(
        glyph_cache: &mut GlyphCache,
        render_context: &mut RenderContext<'_>,
        config: &Config,
        size_info: &SizeInfo,
        meter: &Meter,
    ) {
        if !config.ui_config.debug.render_timer {
            return;
        }

        let timing = format!("{:.3} usec", meter.average());
        let fg = config.colors.primary.background;
        let bg = config.colors.normal().red;

        render_context.render_string(
            glyph_cache,
            size_info.screen_lines() - 2,
            &timing[..],
            fg,
            Some(bg),
        );
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

/// Calculate the size of the window given padding, terminal dimensions and cell size.
fn window_size(
    config: &Config,
    dimensions: Dimensions,
    cell_width: f32,
    cell_height: f32,
    dpr: f64,
) -> PhysicalSize<u32> {
    let padding = config.ui_config.window.padding(dpr);

    let grid_width = cell_width * dimensions.columns.0.max(MIN_COLS) as f32;
    let grid_height = cell_height * dimensions.lines.0.max(MIN_SCREEN_LINES) as f32;

    let width = (padding.0).mul_add(2., grid_width).floor();
    let height = (padding.1).mul_add(2., grid_height).floor();

    PhysicalSize::new(width as u32, height as u32)
}
