//! Exports the `Term` type which is a high-level API for the Grid.

use std::cmp::{max, min};
use std::iter::Peekable;
use std::ops::{Index, IndexMut, Range, RangeInclusive};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{io, iter, mem, ptr, str};

use log::{debug, trace};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

use crate::ansi::{
    self, Attr, CharsetIndex, Color, CursorStyle, Handler, NamedColor, StandardCharset,
};
use crate::config::{BellAnimation, BellConfig, Config};
use crate::event::{Event, EventListener};
use crate::grid::{Dimensions, DisplayIter, Grid, IndexRegion, Indexed, Scroll};
use crate::index::{self, Boundary, Column, Direction, IndexRange, Line, Point, Side};
use crate::selection::{Selection, SelectionRange};
use crate::term::cell::{Cell, Flags, LineLength};
use crate::term::color::{CellRgb, Rgb, DIM_FACTOR};
use crate::term::search::{RegexIter, RegexSearch};
use crate::vi_mode::{ViModeCursor, ViMotion};

pub mod cell;
pub mod color;
mod search;

/// Max size of the window title stack.
const TITLE_STACK_MAX_DEPTH: usize = 4096;

/// Minimum contrast between a fixed cursor color and the cell's background.
const MIN_CURSOR_CONTRAST: f64 = 1.5;

/// Maximum number of linewraps followed outside of the viewport during search highlighting.
const MAX_SEARCH_LINES: usize = 100;

/// Default tab interval, corresponding to terminfo `it` value.
const INITIAL_TABSTOPS: usize = 8;

/// Minimum number of columns.
///
/// A minimum of 2 is necessary to hold fullwidth unicode characters.
pub const MIN_COLS: usize = 2;

/// Minimum number of visible lines.
pub const MIN_SCREEN_LINES: usize = 1;

/// Cursor storing all information relevant for rendering.
#[derive(Debug, Eq, PartialEq, Copy, Clone, Deserialize)]
struct RenderableCursor {
    text_color: CellRgb,
    cursor_color: CellRgb,
    key: CursorKey,
    point: Point,
    rendered: bool,
}

/// A key for caching cursor glyphs.
#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash, Deserialize)]
pub struct CursorKey {
    pub style: CursorStyle,
    pub is_wide: bool,
}

type MatchIter<'a> = Box<dyn Iterator<Item = RangeInclusive<Point<usize>>> + 'a>;

/// Regex search highlight tracking.
pub struct RenderableSearch<'a> {
    iter: Peekable<MatchIter<'a>>,
}

impl<'a> RenderableSearch<'a> {
    /// Create a new renderable search iterator.
    fn new<T>(term: &'a Term<T>) -> Self {
        let viewport_end = term.grid().display_offset();
        let viewport_start = viewport_end + term.screen_lines().0 - 1;

        // Compute start of the first and end of the last line.
        let start_point = Point::new(viewport_start, Column(0));
        let mut start = term.line_search_left(start_point);
        let end_point = Point::new(viewport_end, term.cols() - 1);
        let mut end = term.line_search_right(end_point);

        // Set upper bound on search before/after the viewport to prevent excessive blocking.
        if start.line > viewport_start + MAX_SEARCH_LINES {
            if start.line == 0 {
                // Do not highlight anything if this line is the last.
                let iter: MatchIter<'a> = Box::new(iter::empty());
                return Self { iter: iter.peekable() };
            } else {
                // Start at next line if this one is too long.
                start.line -= 1;
            }
        }
        end.line = max(end.line, viewport_end.saturating_sub(MAX_SEARCH_LINES));

        // Create an iterater for the current regex search for all visible matches.
        let iter: MatchIter<'a> = Box::new(
            RegexIter::new(start, end, Direction::Right, &term)
                .skip_while(move |rm| rm.end().line > viewport_start)
                .take_while(move |rm| rm.start().line >= viewport_end),
        );

        Self { iter: iter.peekable() }
    }

    /// Advance the search tracker to the next point.
    ///
    /// This will return `true` if the point passed is part of a search match.
    fn advance(&mut self, point: Point<usize>) -> bool {
        while let Some(regex_match) = &self.iter.peek() {
            if regex_match.start() > &point {
                break;
            } else if regex_match.end() < &point {
                let _ = self.iter.next();
            } else {
                return true;
            }
        }
        false
    }
}

/// Iterator that yields cells needing render.
///
/// Yields cells that require work to be displayed (that is, not a an empty
/// background cell). Additionally, this manages some state of the grid only
/// relevant for rendering like temporarily changing the cell with the cursor.
///
/// This manages the cursor during a render. The cursor location is inverted to
/// draw it, and reverted after drawing to maintain state.
pub struct RenderableCellsIter<'a, C> {
    inner: DisplayIter<'a, Cell>,
    grid: &'a Grid<Cell>,
    cursor: RenderableCursor,
    config: &'a Config<C>,
    colors: &'a color::List,
    selection: Option<SelectionRange<Line>>,
    search: RenderableSearch<'a>,
}

impl<'a, C> RenderableCellsIter<'a, C> {
    /// Create the renderable cells iterator.
    ///
    /// The cursor and terminal mode are required for properly displaying the
    /// cursor.
    fn new<T>(
        term: &'a Term<T>,
        config: &'a Config<C>,
        selection: Option<SelectionRange>,
    ) -> RenderableCellsIter<'a, C> {
        let grid = &term.grid;

        let selection_range = selection.and_then(|span| {
            let (limit_start, limit_end) = if span.is_block {
                (span.start.col, span.end.col)
            } else {
                (Column(0), grid.cols() - 1)
            };

            // Do not render completely offscreen selection.
            let viewport_end = grid.display_offset();
            let viewport_start = viewport_end + grid.screen_lines().0 - 1;
            if span.end.line > viewport_start || span.start.line < viewport_end {
                return None;
            }

            // Get on-screen lines of the selection's locations.
            let mut start = grid.clamp_buffer_to_visible(span.start);
            let mut end = grid.clamp_buffer_to_visible(span.end);

            // Trim start/end with partially visible block selection.
            start.col = max(limit_start, start.col);
            end.col = min(limit_end, end.col);

            Some(SelectionRange::new(start, end, span.is_block))
        });

        RenderableCellsIter {
            cursor: term.renderable_cursor(config),
            grid,
            inner: grid.display_iter(),
            selection: selection_range,
            config,
            colors: &term.colors,
            search: RenderableSearch::new(term),
        }
    }

    /// Check selection state of a cell.
    fn is_selected(&self, point: Point) -> bool {
        let selection = match self.selection {
            Some(selection) => selection,
            None => return false,
        };

        // Do not invert block cursor at selection boundaries.
        if self.cursor.key.style == CursorStyle::Block
            && self.cursor.point == point
            && (selection.start == point
                || selection.end == point
                || (selection.is_block
                    && ((selection.start.line == point.line && selection.end.col == point.col)
                        || (selection.end.line == point.line && selection.start.col == point.col))))
        {
            return false;
        }

        // Point itself is selected.
        if selection.contains(point.col, point.line) {
            return true;
        }

        let num_cols = self.grid.cols();

        // Convert to absolute coordinates to adjust for the display offset.
        let buffer_point = self.grid.visible_to_buffer(point);
        let cell = self.grid[buffer_point];

        // Check if wide char's spacers are selected.
        if cell.flags.contains(Flags::WIDE_CHAR) {
            let prev = point.sub(num_cols, 1);
            let buffer_prev = self.grid.visible_to_buffer(prev);
            let next = point.add(num_cols, 1);

            // Check trailing spacer.
            selection.contains(next.col, next.line)
                // Check line-wrapping, leading spacer.
                || (self.grid[buffer_prev].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                    && selection.contains(prev.col, prev.line))
        } else if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            // Check if spacer's wide char is selected.
            let prev = point.sub(num_cols, 1);
            let buffer_prev = self.grid.visible_to_buffer(prev);

            if self.grid[buffer_prev].flags.contains(Flags::WIDE_CHAR) {
                // Check previous cell for trailing spacer.
                self.is_selected(prev)
            } else {
                // Check next cell for line-wrapping, leading spacer.
                self.is_selected(point.add(num_cols, 1))
            }
        } else {
            false
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RenderableCellContent {
    Chars([char; cell::MAX_ZEROWIDTH_CHARS + 1]),
    Cursor(CursorKey),
}

#[derive(Copy, Clone, Debug)]
pub struct RenderableCell {
    /// A _Display_ line (not necessarily an _Active_ line).
    pub line: Line,
    pub column: Column,
    pub inner: RenderableCellContent,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bg_alpha: f32,
    pub flags: Flags,
}

impl RenderableCell {
    fn new<'a, C>(iter: &mut RenderableCellsIter<'a, C>, cell: Indexed<Cell>) -> Self {
        let point = Point::new(cell.line, cell.column);

        // Lookup RGB values.
        let mut fg_rgb = Self::compute_fg_rgb(iter.config, iter.colors, cell.fg, cell.flags);
        let mut bg_rgb = Self::compute_bg_rgb(iter.colors, cell.bg);

        let mut bg_alpha = if cell.inverse() {
            mem::swap(&mut fg_rgb, &mut bg_rgb);
            1.0
        } else {
            Self::compute_bg_alpha(cell.bg)
        };

        if iter.is_selected(point) {
            let config_bg = iter.config.colors.selection.background();
            let selected_fg = iter.config.colors.selection.text().color(fg_rgb, bg_rgb);
            bg_rgb = config_bg.color(fg_rgb, bg_rgb);
            fg_rgb = selected_fg;

            if fg_rgb == bg_rgb && !cell.flags.contains(Flags::HIDDEN) {
                // Reveal inversed text when fg/bg is the same.
                fg_rgb = iter.colors[NamedColor::Background];
                bg_rgb = iter.colors[NamedColor::Foreground];
                bg_alpha = 1.0;
            } else if config_bg != CellRgb::CellBackground {
                bg_alpha = 1.0;
            }
        } else if iter.search.advance(iter.grid.visible_to_buffer(point)) {
            // Highlight the cell if it is part of a search match.
            let config_bg = iter.config.colors.search.matches.background;
            let matched_fg = iter.config.colors.search.matches.foreground.color(fg_rgb, bg_rgb);
            bg_rgb = config_bg.color(fg_rgb, bg_rgb);
            fg_rgb = matched_fg;

            if config_bg != CellRgb::CellBackground {
                bg_alpha = 1.0;
            }
        }

        RenderableCell {
            line: cell.line,
            column: cell.column,
            inner: RenderableCellContent::Chars(cell.chars()),
            fg: fg_rgb,
            bg: bg_rgb,
            bg_alpha,
            flags: cell.flags,
        }
    }

    fn is_empty(&self) -> bool {
        self.bg_alpha == 0.
            && !self.flags.intersects(Flags::UNDERLINE | Flags::STRIKEOUT | Flags::DOUBLE_UNDERLINE)
            && self.inner == RenderableCellContent::Chars([' '; cell::MAX_ZEROWIDTH_CHARS + 1])
    }

    fn compute_fg_rgb<C>(config: &Config<C>, colors: &color::List, fg: Color, flags: Flags) -> Rgb {
        match fg {
            Color::Spec(rgb) => match flags & Flags::DIM {
                Flags::DIM => rgb * DIM_FACTOR,
                _ => rgb,
            },
            Color::Named(ansi) => {
                match (config.draw_bold_text_with_bright_colors(), flags & Flags::DIM_BOLD) {
                    // If no bright foreground is set, treat it like the BOLD flag doesn't exist.
                    (_, Flags::DIM_BOLD)
                        if ansi == NamedColor::Foreground
                            && config.colors.primary.bright_foreground.is_none() =>
                    {
                        colors[NamedColor::DimForeground]
                    },
                    // Draw bold text in bright colors *and* contains bold flag.
                    (true, Flags::BOLD) => colors[ansi.to_bright()],
                    // Cell is marked as dim and not bold.
                    (_, Flags::DIM) | (false, Flags::DIM_BOLD) => colors[ansi.to_dim()],
                    // None of the above, keep original color..
                    _ => colors[ansi],
                }
            },
            Color::Indexed(idx) => {
                let idx = match (
                    config.draw_bold_text_with_bright_colors(),
                    flags & Flags::DIM_BOLD,
                    idx,
                ) {
                    (true, Flags::BOLD, 0..=7) => idx as usize + 8,
                    (false, Flags::DIM, 8..=15) => idx as usize - 8,
                    (false, Flags::DIM, 0..=7) => idx as usize + 260,
                    _ => idx as usize,
                };

                colors[idx]
            },
        }
    }

    /// Compute background alpha based on cell's original color.
    ///
    /// Since an RGB color matching the background should not be transparent, this is computed
    /// using the named input color, rather than checking the RGB of the background after its color
    /// is computed.
    #[inline]
    fn compute_bg_alpha(bg: Color) -> f32 {
        if bg == Color::Named(NamedColor::Background) {
            0.
        } else {
            1.
        }
    }

    #[inline]
    fn compute_bg_rgb(colors: &color::List, bg: Color) -> Rgb {
        match bg {
            Color::Spec(rgb) => rgb,
            Color::Named(ansi) => colors[ansi],
            Color::Indexed(idx) => colors[idx],
        }
    }
}

impl<'a, C> Iterator for RenderableCellsIter<'a, C> {
    type Item = RenderableCell;

    /// Gets the next renderable cell.
    ///
    /// Skips empty (background) cells and applies any flags to the cell state
    /// (eg. invert fg and bg colors).
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.cursor.point.line == self.inner.line()
                && self.cursor.point.col == self.inner.column()
            {
                if self.cursor.rendered {
                    // Handle cell below cursor.
                    let cell = self.inner.next()?;
                    let mut cell = RenderableCell::new(self, cell);

                    if self.cursor.key.style == CursorStyle::Block {
                        cell.fg = match self.cursor.cursor_color {
                            // Apply cursor color, or invert the cursor if it has a fixed background
                            // close to the cell's background.
                            CellRgb::Rgb(col) if col.contrast(cell.bg) < MIN_CURSOR_CONTRAST => {
                                cell.bg
                            },
                            _ => self.cursor.text_color.color(cell.fg, cell.bg),
                        };
                    }

                    return Some(cell);
                } else {
                    // Handle cursor.
                    self.cursor.rendered = true;

                    let buffer_point = self.grid.visible_to_buffer(self.cursor.point);
                    let cell = Indexed {
                        inner: self.grid[buffer_point.line][buffer_point.col],
                        column: self.cursor.point.col,
                        line: self.cursor.point.line,
                    };

                    let mut cell = RenderableCell::new(self, cell);
                    cell.inner = RenderableCellContent::Cursor(self.cursor.key);

                    // Apply cursor color, or invert the cursor if it has a fixed background close
                    // to the cell's background.
                    if !matches!(
                        self.cursor.cursor_color,
                        CellRgb::Rgb(color) if color.contrast(cell.bg) < MIN_CURSOR_CONTRAST
                    ) {
                        cell.fg = self.cursor.cursor_color.color(cell.fg, cell.bg);
                    }

                    return Some(cell);
                }
            } else {
                let cell = self.inner.next()?;
                let cell = RenderableCell::new(self, cell);

                if !cell.is_empty() {
                    return Some(cell);
                }
            }
        }
    }
}

pub mod mode {
    use bitflags::bitflags;

    bitflags! {
        pub struct TermMode: u32 {
            const NONE                = 0;
            const SHOW_CURSOR         = 0b0000_0000_0000_0000_0001;
            const APP_CURSOR          = 0b0000_0000_0000_0000_0010;
            const APP_KEYPAD          = 0b0000_0000_0000_0000_0100;
            const MOUSE_REPORT_CLICK  = 0b0000_0000_0000_0000_1000;
            const BRACKETED_PASTE     = 0b0000_0000_0000_0001_0000;
            const SGR_MOUSE           = 0b0000_0000_0000_0010_0000;
            const MOUSE_MOTION        = 0b0000_0000_0000_0100_0000;
            const LINE_WRAP           = 0b0000_0000_0000_1000_0000;
            const LINE_FEED_NEW_LINE  = 0b0000_0000_0001_0000_0000;
            const ORIGIN              = 0b0000_0000_0010_0000_0000;
            const INSERT              = 0b0000_0000_0100_0000_0000;
            const FOCUS_IN_OUT        = 0b0000_0000_1000_0000_0000;
            const ALT_SCREEN          = 0b0000_0001_0000_0000_0000;
            const MOUSE_DRAG          = 0b0000_0010_0000_0000_0000;
            const MOUSE_MODE          = 0b0000_0010_0000_0100_1000;
            const UTF8_MOUSE          = 0b0000_0100_0000_0000_0000;
            const ALTERNATE_SCROLL    = 0b0000_1000_0000_0000_0000;
            const VI                  = 0b0001_0000_0000_0000_0000;
            const URGENCY_HINTS       = 0b0010_0000_0000_0000_0000;
            const ANY                 = std::u32::MAX;
        }
    }

    impl Default for TermMode {
        fn default() -> TermMode {
            TermMode::SHOW_CURSOR
                | TermMode::LINE_WRAP
                | TermMode::ALTERNATE_SCROLL
                | TermMode::URGENCY_HINTS
        }
    }
}

pub use crate::term::mode::TermMode;

pub struct VisualBell {
    /// Visual bell animation.
    animation: BellAnimation,

    /// Visual bell duration.
    duration: Duration,

    /// The last time the visual bell rang, if at all.
    start_time: Option<Instant>,
}

fn cubic_bezier(p0: f64, p1: f64, p2: f64, p3: f64, x: f64) -> f64 {
    (1.0 - x).powi(3) * p0
        + 3.0 * (1.0 - x).powi(2) * x * p1
        + 3.0 * (1.0 - x) * x.powi(2) * p2
        + x.powi(3) * p3
}

impl VisualBell {
    /// Ring the visual bell, and return its intensity.
    pub fn ring(&mut self) -> f64 {
        let now = Instant::now();
        self.start_time = Some(now);
        self.intensity_at_instant(now)
    }

    /// Get the currently intensity of the visual bell. The bell's intensity
    /// ramps down from 1.0 to 0.0 at a rate determined by the bell's duration.
    pub fn intensity(&self) -> f64 {
        self.intensity_at_instant(Instant::now())
    }

    /// Check whether or not the visual bell has completed "ringing".
    pub fn completed(&mut self) -> bool {
        match self.start_time {
            Some(earlier) => {
                if Instant::now().duration_since(earlier) >= self.duration {
                    self.start_time = None;
                }
                false
            },
            None => true,
        }
    }

    /// Get the intensity of the visual bell at a particular instant. The bell's
    /// intensity ramps down from 1.0 to 0.0 at a rate determined by the bell's
    /// duration.
    pub fn intensity_at_instant(&self, instant: Instant) -> f64 {
        // If `duration` is zero, then the VisualBell is disabled; therefore,
        // its `intensity` is zero.
        if self.duration == Duration::from_secs(0) {
            return 0.0;
        }

        match self.start_time {
            // Similarly, if `start_time` is `None`, then the VisualBell has not
            // been "rung"; therefore, its `intensity` is zero.
            None => 0.0,

            Some(earlier) => {
                // Finally, if the `instant` at which we wish to compute the
                // VisualBell's `intensity` occurred before the VisualBell was
                // "rung", then its `intensity` is also zero.
                if instant < earlier {
                    return 0.0;
                }

                let elapsed = instant.duration_since(earlier);
                let elapsed_f =
                    elapsed.as_secs() as f64 + f64::from(elapsed.subsec_nanos()) / 1e9f64;
                let duration_f = self.duration.as_secs() as f64
                    + f64::from(self.duration.subsec_nanos()) / 1e9f64;

                // Otherwise, we compute a value `time` from 0.0 to 1.0
                // inclusive that represents the ratio of `elapsed` time to the
                // `duration` of the VisualBell.
                let time = (elapsed_f / duration_f).min(1.0);

                // We use this to compute the inverse `intensity` of the
                // VisualBell. When `time` is 0.0, `inverse_intensity` is 0.0,
                // and when `time` is 1.0, `inverse_intensity` is 1.0.
                let inverse_intensity = match self.animation {
                    BellAnimation::Ease | BellAnimation::EaseOut => {
                        cubic_bezier(0.25, 0.1, 0.25, 1.0, time)
                    },
                    BellAnimation::EaseOutSine => cubic_bezier(0.39, 0.575, 0.565, 1.0, time),
                    BellAnimation::EaseOutQuad => cubic_bezier(0.25, 0.46, 0.45, 0.94, time),
                    BellAnimation::EaseOutCubic => cubic_bezier(0.215, 0.61, 0.355, 1.0, time),
                    BellAnimation::EaseOutQuart => cubic_bezier(0.165, 0.84, 0.44, 1.0, time),
                    BellAnimation::EaseOutQuint => cubic_bezier(0.23, 1.0, 0.32, 1.0, time),
                    BellAnimation::EaseOutExpo => cubic_bezier(0.19, 1.0, 0.22, 1.0, time),
                    BellAnimation::EaseOutCirc => cubic_bezier(0.075, 0.82, 0.165, 1.0, time),
                    BellAnimation::Linear => time,
                };

                // Since we want the `intensity` of the VisualBell to decay over
                // `time`, we subtract the `inverse_intensity` from 1.0.
                1.0 - inverse_intensity
            },
        }
    }

    pub fn update_config<C>(&mut self, config: &Config<C>) {
        let bell_config = config.bell();
        self.animation = bell_config.animation;
        self.duration = bell_config.duration();
    }
}

impl From<&BellConfig> for VisualBell {
    fn from(bell_config: &BellConfig) -> VisualBell {
        VisualBell {
            animation: bell_config.animation,
            duration: bell_config.duration(),
            start_time: None,
        }
    }
}

/// Terminal size info.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq)]
pub struct SizeInfo {
    /// Terminal window width.
    width: f32,

    /// Terminal window height.
    height: f32,

    /// Width of individual cell.
    cell_width: f32,

    /// Height of individual cell.
    cell_height: f32,

    /// Horizontal window padding.
    padding_x: f32,

    /// Horizontal window padding.
    padding_y: f32,

    /// Number of lines in the viewport.
    screen_lines: Line,

    /// Number of total visible lines, including screen_lines and reserved lines.
    visible_lines: Line,

    /// Number of columns in the viewport.
    cols: Column,
}

impl SizeInfo {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        width: f32,
        height: f32,
        cell_width: f32,
        cell_height: f32,
        mut padding_x: f32,
        mut padding_y: f32,
        dynamic_padding: bool,
    ) -> SizeInfo {
        if dynamic_padding {
            padding_x = Self::dynamic_padding(padding_x.floor(), width, cell_width);
            padding_y = Self::dynamic_padding(padding_y.floor(), height, cell_height);
        }

        let lines = (height - 2. * padding_y) / cell_height;
        let screen_lines = Line(max(lines as usize, MIN_SCREEN_LINES));

        let cols = (width - 2. * padding_x) / cell_width;
        let cols = Column(max(cols as usize, MIN_COLS));

        SizeInfo {
            width,
            height,
            cell_width,
            cell_height,
            padding_x: padding_x.floor(),
            padding_y: padding_y.floor(),
            screen_lines,
            visible_lines: screen_lines,
            cols,
        }
    }

    #[inline]
    pub fn reserve_lines(&mut self, count: usize) {
        self.screen_lines = Line(max(self.screen_lines.saturating_sub(count), MIN_SCREEN_LINES));
    }

    /// Check if coordinates are inside the terminal grid.
    ///
    /// The padding, message bar or search are not counted as part of the grid.
    #[inline]
    pub fn contains_point(&self, x: usize, y: usize) -> bool {
        x <= (self.padding_x + self.cols.0 as f32 * self.cell_width) as usize
            && x > self.padding_x as usize
            && y <= (self.padding_y + self.screen_lines.0 as f32 * self.cell_height) as usize
            && y > self.padding_y as usize
    }

    /// Convert window space pixels to terminal grid coordinates.
    ///
    /// If the coordinates are outside of the terminal grid, like positions inside the padding, the
    /// coordinates will be clamped to the closest grid coordinates.
    pub fn pixels_to_coords(&self, x: usize, y: usize) -> Point {
        let col = Column(x.saturating_sub(self.padding_x as usize) / (self.cell_width as usize));
        let line = Line(y.saturating_sub(self.padding_y as usize) / (self.cell_height as usize));

        Point {
            line: min(line, Line(self.screen_lines.saturating_sub(1))),
            col: min(col, Column(self.cols.saturating_sub(1))),
        }
    }

    #[inline]
    pub fn width(&self) -> f32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> f32 {
        self.height
    }

    #[inline]
    pub fn cell_width(&self) -> f32 {
        self.cell_width
    }

    #[inline]
    pub fn cell_height(&self) -> f32 {
        self.cell_height
    }

    #[inline]
    pub fn padding_x(&self) -> f32 {
        self.padding_x
    }

    #[inline]
    pub fn padding_y(&self) -> f32 {
        self.padding_y
    }

    #[inline]
    pub fn screen_lines(&self) -> Line {
        self.screen_lines
    }

    #[inline]
    pub fn visible_lines(&self) -> Line {
        self.visible_lines
    }

    #[inline]
    pub fn cols(&self) -> Column {
        self.cols
    }

    /// Calculate padding to spread it evenly around the terminal content.
    #[inline]
    fn dynamic_padding(padding: f32, dimension: f32, cell_dimension: f32) -> f32 {
        padding + ((dimension - 2. * padding) % cell_dimension) / 2.
    }
}

pub struct Term<T> {
    /// Terminal requires redraw.
    pub dirty: bool,

    /// Visual bell configuration and status.
    pub visual_bell: VisualBell,

    /// Terminal focus controlling the cursor shape.
    pub is_focused: bool,

    /// Cursor for keyboard selection.
    pub vi_mode_cursor: ViModeCursor,

    pub selection: Option<Selection>,

    /// Currently active grid.
    ///
    /// Tracks the screen buffer currently in use. While the alternate screen buffer is active,
    /// this will be the alternate grid. Otherwise it is the primary screen buffer.
    grid: Grid<Cell>,

    /// Currently inactive grid.
    ///
    /// Opposite of the active grid. While the alternate screen buffer is active, this will be the
    /// primary grid. Otherwise it is the alternate screen buffer.
    inactive_grid: Grid<Cell>,

    /// Index into `charsets`, pointing to what ASCII is currently being mapped to.
    active_charset: CharsetIndex,

    /// Tabstops.
    tabs: TabStops,

    /// Mode flags.
    mode: TermMode,

    /// Scroll region.
    ///
    /// Range going from top to bottom of the terminal, indexed from the top of the viewport.
    scroll_region: Range<Line>,

    semantic_escape_chars: String,

    /// Colors used for rendering.
    colors: color::List,

    /// Is color in `colors` modified or not.
    color_modified: [bool; color::COUNT],

    /// Original colors from config.
    original_colors: color::List,

    /// Current style of the cursor.
    cursor_style: Option<CursorStyle>,

    /// Default style for resetting the cursor.
    default_cursor_style: CursorStyle,

    /// Style of the vi mode cursor.
    vi_mode_cursor_style: Option<CursorStyle>,

    /// Proxy for sending events to the event loop.
    event_proxy: T,

    /// Current title of the window.
    title: Option<String>,

    /// Stack of saved window titles. When a title is popped from this stack, the `title` for the
    /// term is set.
    title_stack: Vec<Option<String>>,

    /// Current forward and backward buffer search regexes.
    regex_search: Option<RegexSearch>,

    /// Information about cell dimensions.
    cell_width: usize,
    cell_height: usize,
}

impl<T> Term<T> {
    #[inline]
    pub fn scroll_display(&mut self, scroll: Scroll)
    where
        T: EventListener,
    {
        self.grid.scroll_display(scroll);
        self.event_proxy.send_event(Event::MouseCursorDirty);
        self.dirty = true;
    }

    pub fn new<C>(config: &Config<C>, size: SizeInfo, event_proxy: T) -> Term<T> {
        let num_cols = size.cols;
        let num_lines = size.screen_lines;

        let history_size = config.scrolling.history() as usize;
        let grid = Grid::new(num_lines, num_cols, history_size, Cell::default());
        let alt = Grid::new(num_lines, num_cols, 0 /* scroll history */, Cell::default());

        let tabs = TabStops::new(grid.cols());

        let scroll_region = Line(0)..grid.screen_lines();

        let colors = color::List::from(&config.colors);

        Term {
            dirty: false,
            visual_bell: config.bell().into(),
            grid,
            inactive_grid: alt,
            active_charset: Default::default(),
            vi_mode_cursor: Default::default(),
            tabs,
            mode: Default::default(),
            scroll_region,
            colors,
            color_modified: [false; color::COUNT],
            original_colors: colors,
            semantic_escape_chars: config.selection.semantic_escape_chars().to_owned(),
            cursor_style: None,
            default_cursor_style: config.cursor.style,
            vi_mode_cursor_style: config.cursor.vi_mode_style,
            event_proxy,
            is_focused: true,
            title: None,
            title_stack: Vec::new(),
            selection: None,
            regex_search: None,
            cell_width: size.cell_width as usize,
            cell_height: size.cell_height as usize,
        }
    }

    pub fn update_config<C>(&mut self, config: &Config<C>)
    where
        T: EventListener,
    {
        self.semantic_escape_chars = config.selection.semantic_escape_chars().to_owned();
        self.original_colors.fill_named(&config.colors);
        self.original_colors.fill_cube(&config.colors);
        self.original_colors.fill_gray_ramp(&config.colors);
        for i in 0..color::COUNT {
            if !self.color_modified[i] {
                self.colors[i] = self.original_colors[i];
            }
        }
        self.visual_bell.update_config(config);
        if let Some(0) = config.scrolling.faux_multiplier() {
            self.mode.remove(TermMode::ALTERNATE_SCROLL);
        }
        self.default_cursor_style = config.cursor.style;
        self.vi_mode_cursor_style = config.cursor.vi_mode_style;

        let title_event = match &self.title {
            Some(title) => Event::Title(title.clone()),
            None => Event::ResetTitle,
        };

        self.event_proxy.send_event(title_event);

        if self.mode.contains(TermMode::ALT_SCREEN) {
            self.inactive_grid.update_history(config.scrolling.history() as usize);
        } else {
            self.grid.update_history(config.scrolling.history() as usize);
        }
    }

    /// Convert the active selection to a String.
    pub fn selection_to_string(&self) -> Option<String> {
        let selection_range = self.selection.as_ref().and_then(|s| s.to_range(self))?;
        let SelectionRange { start, end, is_block } = selection_range;

        let mut res = String::new();

        if is_block {
            for line in (end.line + 1..=start.line).rev() {
                res += &self.line_to_string(line, start.col..end.col, start.col.0 != 0);

                // If the last column is included, newline is appended automatically.
                if end.col != self.cols() - 1 {
                    res += "\n";
                }
            }
            res += &self.line_to_string(end.line, start.col..end.col, true);
        } else {
            res = self.bounds_to_string(start, end);
        }

        Some(res)
    }

    /// Convert range between two points to a String.
    pub fn bounds_to_string(&self, start: Point<usize>, end: Point<usize>) -> String {
        let mut res = String::new();

        for line in (end.line..=start.line).rev() {
            let start_col = if line == start.line { start.col } else { Column(0) };
            let end_col = if line == end.line { end.col } else { self.cols() - 1 };

            res += &self.line_to_string(line, start_col..end_col, line == end.line);
        }

        res
    }

    /// Convert a single line in the grid to a String.
    fn line_to_string(
        &self,
        line: usize,
        mut cols: Range<Column>,
        include_wrapped_wide: bool,
    ) -> String {
        let mut text = String::new();

        let grid_line = &self.grid[line];
        let line_length = min(grid_line.line_length(), cols.end + 1);

        // Include wide char when trailing spacer is selected.
        if grid_line[cols.start].flags.contains(Flags::WIDE_CHAR_SPACER) {
            cols.start -= 1;
        }

        let mut tab_mode = false;
        for col in IndexRange::from(cols.start..line_length) {
            let cell = grid_line[col];

            // Skip over cells until next tab-stop once a tab was found.
            if tab_mode {
                if self.tabs[col] {
                    tab_mode = false;
                } else {
                    continue;
                }
            }

            if cell.c == '\t' {
                tab_mode = true;
            }

            if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                // Push cells primary character.
                text.push(cell.c);

                // Push zero-width characters.
                for c in (&cell.chars()[1..]).iter().take_while(|c| **c != ' ') {
                    text.push(*c);
                }
            }
        }

        if cols.end >= self.cols() - 1
            && (line_length.0 == 0
                || !self.grid[line][line_length - 1].flags.contains(Flags::WRAPLINE))
        {
            text.push('\n');
        }

        // If wide char is not part of the selection, but leading spacer is, include it.
        if line_length == self.cols()
            && line_length.0 >= 2
            && grid_line[line_length - 1].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            && include_wrapped_wide
        {
            text.push(self.grid[line - 1][Column(0)].c);
        }

        text
    }

    pub fn visible_to_buffer(&self, point: Point) -> Point<usize> {
        self.grid.visible_to_buffer(point)
    }

    /// Access to the raw grid data structure.
    ///
    /// This is a bit of a hack; when the window is closed, the event processor
    /// serializes the grid state to a file.
    pub fn grid(&self) -> &Grid<Cell> {
        &self.grid
    }

    /// Mutable access for swapping out the grid during tests.
    #[cfg(test)]
    pub fn grid_mut(&mut self) -> &mut Grid<Cell> {
        &mut self.grid
    }

    /// Iterate over the *renderable* cells in the terminal.
    ///
    /// A renderable cell is any cell which has content other than the default
    /// background color.  Cells with an alternate background color are
    /// considered renderable as are cells with any text content.
    pub fn renderable_cells<'b, C>(&'b self, config: &'b Config<C>) -> RenderableCellsIter<'_, C> {
        let selection = self.selection.as_ref().and_then(|s| s.to_range(self));

        RenderableCellsIter::new(&self, config, selection)
    }

    /// Resize terminal to new dimensions.
    pub fn resize(&mut self, size: SizeInfo) {
        self.cell_width = size.cell_width as usize;
        self.cell_height = size.cell_height as usize;

        let old_cols = self.cols();
        let old_lines = self.screen_lines();

        let num_cols = size.cols;
        let num_lines = size.screen_lines;

        if old_cols == num_cols && old_lines == num_lines {
            debug!("Term::resize dimensions unchanged");
            return;
        }

        debug!("New num_cols is {} and num_lines is {}", num_cols, num_lines);

        // Invalidate selection and tabs only when necessary.
        if old_cols != num_cols {
            self.selection = None;

            // Recreate tabs list.
            self.tabs.resize(num_cols);
        } else if let Some(selection) = self.selection.take() {
            // Move the selection if only number of lines changed.
            let delta = if num_lines > old_lines {
                (num_lines - old_lines.0).saturating_sub(self.history_size()) as isize
            } else {
                let cursor_line = self.grid.cursor.point.line;
                -(min(old_lines - cursor_line - 1, old_lines - num_lines).0 as isize)
            };
            self.selection = selection.rotate(self, &(Line(0)..num_lines), delta);
        }

        let is_alt = self.mode.contains(TermMode::ALT_SCREEN);

        self.grid.resize(!is_alt, num_lines, num_cols);
        self.inactive_grid.resize(is_alt, num_lines, num_cols);

        // Clamp vi cursor to viewport.
        self.vi_mode_cursor.point.col = min(self.vi_mode_cursor.point.col, num_cols - 1);
        self.vi_mode_cursor.point.line = min(self.vi_mode_cursor.point.line, num_lines - 1);

        // Reset scrolling region.
        self.scroll_region = Line(0)..self.screen_lines();
    }

    /// Active terminal modes.
    #[inline]
    pub fn mode(&self) -> &TermMode {
        &self.mode
    }

    /// Swap primary and alternate screen buffer.
    pub fn swap_alt(&mut self) {
        if !self.mode.contains(TermMode::ALT_SCREEN) {
            // Set alt screen cursor to the current primary screen cursor.
            self.inactive_grid.cursor = self.grid.cursor;

            // Drop information about the primary screens saved cursor.
            self.grid.saved_cursor = self.grid.cursor;

            // Reset alternate screen contents.
            let template = self.inactive_grid.cursor.template;
            self.inactive_grid.region_mut(..).each(|c| c.reset(&template));
        }

        mem::swap(&mut self.grid, &mut self.inactive_grid);
        self.mode ^= TermMode::ALT_SCREEN;
        self.selection = None;
    }

    /// Scroll screen down.
    ///
    /// Text moves down; clear at bottom
    /// Expects origin to be in scroll range.
    #[inline]
    fn scroll_down_relative(&mut self, origin: Line, mut lines: Line) {
        trace!("Scrolling down relative: origin={}, lines={}", origin, lines);

        let num_lines = self.screen_lines();

        lines = min(lines, self.scroll_region.end - self.scroll_region.start);
        lines = min(lines, self.scroll_region.end - origin);

        let region = origin..self.scroll_region.end;
        let absolute_region = (num_lines - region.end)..(num_lines - region.start);

        // Scroll selection.
        self.selection = self
            .selection
            .take()
            .and_then(|s| s.rotate(self, &absolute_region, -(lines.0 as isize)));

        // Scroll between origin and bottom
        let template = Cell { bg: self.grid.cursor.template.bg, ..Cell::default() };
        self.grid.scroll_down(&region, lines, template);
    }

    /// Scroll screen up
    ///
    /// Text moves up; clear at top
    /// Expects origin to be in scroll range.
    #[inline]
    fn scroll_up_relative(&mut self, origin: Line, mut lines: Line) {
        trace!("Scrolling up relative: origin={}, lines={}", origin, lines);

        let num_lines = self.screen_lines();

        lines = min(lines, self.scroll_region.end - self.scroll_region.start);

        let region = origin..self.scroll_region.end;
        let absolute_region = (num_lines - region.end)..(num_lines - region.start);

        // Scroll selection.
        self.selection =
            self.selection.take().and_then(|s| s.rotate(self, &absolute_region, lines.0 as isize));

        // Scroll from origin to bottom less number of lines.
        let template = Cell { bg: self.grid.cursor.template.bg, ..Cell::default() };
        self.grid.scroll_up(&region, lines, template);
    }

    fn deccolm(&mut self)
    where
        T: EventListener,
    {
        // Setting 132 column font makes no sense, but run the other side effects.
        // Clear scrolling region.
        self.set_scrolling_region(1, None);

        // Clear grid.
        let template = self.grid.cursor.template;
        self.grid.region_mut(..).each(|c| c.reset(&template));
    }

    #[inline]
    pub fn background_color(&self) -> Rgb {
        self.colors[NamedColor::Background]
    }

    #[inline]
    pub fn exit(&mut self)
    where
        T: EventListener,
    {
        self.event_proxy.send_event(Event::Exit);
    }

    /// Toggle the vi mode.
    #[inline]
    pub fn toggle_vi_mode(&mut self) {
        self.mode ^= TermMode::VI;

        let vi_mode = self.mode.contains(TermMode::VI);

        // Do not clear selection when entering search.
        if self.regex_search.is_none() || !vi_mode {
            self.selection = None;
        }

        if vi_mode {
            // Reset vi mode cursor position to match primary cursor.
            let cursor = self.grid.cursor.point;
            let line = min(cursor.line + self.grid.display_offset(), self.screen_lines() - 1);
            self.vi_mode_cursor = ViModeCursor::new(Point::new(line, cursor.col));
        } else {
            self.cancel_search();
        }

        self.dirty = true;
    }

    /// Move vi mode cursor.
    #[inline]
    pub fn vi_motion(&mut self, motion: ViMotion)
    where
        T: EventListener,
    {
        // Require vi mode to be active.
        if !self.mode.contains(TermMode::VI) {
            return;
        }

        // Move cursor.
        self.vi_mode_cursor = self.vi_mode_cursor.motion(self, motion);
        self.vi_mode_recompute_selection();

        self.dirty = true;
    }

    /// Move vi cursor to absolute point in grid.
    #[inline]
    pub fn vi_goto_point(&mut self, point: Point<usize>)
    where
        T: EventListener,
    {
        // Move viewport to make point visible.
        self.scroll_to_point(point);

        // Move vi cursor to the point.
        self.vi_mode_cursor.point = self.grid.clamp_buffer_to_visible(point);

        self.vi_mode_recompute_selection();

        self.dirty = true;
    }

    /// Update the active selection to match the vi mode cursor position.
    #[inline]
    fn vi_mode_recompute_selection(&mut self) {
        // Require vi mode to be active.
        if !self.mode.contains(TermMode::VI) {
            return;
        }

        let viewport_point = self.visible_to_buffer(self.vi_mode_cursor.point);

        // Update only if non-empty selection is present.
        let selection = match &mut self.selection {
            Some(selection) if !selection.is_empty() => selection,
            _ => return,
        };

        selection.update(viewport_point, Side::Left);
        selection.include_all();
    }

    /// Scroll display to point if it is outside of viewport.
    pub fn scroll_to_point(&mut self, point: Point<usize>)
    where
        T: EventListener,
    {
        let display_offset = self.grid.display_offset();
        let num_lines = self.screen_lines().0;

        if point.line >= display_offset + num_lines {
            let lines = point.line.saturating_sub(display_offset + num_lines - 1);
            self.scroll_display(Scroll::Delta(lines as isize));
        } else if point.line < display_offset {
            let lines = display_offset.saturating_sub(point.line);
            self.scroll_display(Scroll::Delta(-(lines as isize)));
        }
    }

    /// Jump to the end of a wide cell.
    pub fn expand_wide(&self, mut point: Point<usize>, direction: Direction) -> Point<usize> {
        let flags = self.grid[point.line][point.col].flags;

        match direction {
            Direction::Right if flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) => {
                point.col = Column(1);
                point.line -= 1;
            },
            Direction::Right if flags.contains(Flags::WIDE_CHAR) => point.col += 1,
            Direction::Left if flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) => {
                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                    point.col -= 1;
                }

                let prev = point.sub_absolute(self, Boundary::Clamp, 1);
                if self.grid[prev].flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                    point = prev;
                }
            },
            _ => (),
        }

        point
    }

    #[inline]
    pub fn semantic_escape_chars(&self) -> &str {
        &self.semantic_escape_chars
    }

    /// Insert a linebreak at the current cursor position.
    #[inline]
    fn wrapline(&mut self)
    where
        T: EventListener,
    {
        if !self.mode.contains(TermMode::LINE_WRAP) {
            return;
        }

        trace!("Wrapping input");

        self.grid.cursor_cell().flags.insert(Flags::WRAPLINE);

        if (self.grid.cursor.point.line + 1) >= self.scroll_region.end {
            self.linefeed();
        } else {
            self.grid.cursor.point.line += 1;
        }

        self.grid.cursor.point.col = Column(0);
        self.grid.cursor.input_needs_wrap = false;
    }

    /// Write `c` to the cell at the cursor position.
    #[inline]
    fn write_at_cursor(&mut self, c: char) -> &mut Cell
    where
        T: EventListener,
    {
        let mut cell = self.grid.cursor.template;
        cell.c = self.grid.cursor.charsets[self.active_charset].map(c);

        let cursor_cell = self.grid.cursor_cell();
        *cursor_cell = cell;

        cursor_cell
    }

    /// Get rendering information about the active cursor.
    fn renderable_cursor<C>(&self, config: &Config<C>) -> RenderableCursor {
        let vi_mode = self.mode.contains(TermMode::VI);

        // Cursor position.
        let mut point = if vi_mode {
            self.vi_mode_cursor.point
        } else {
            let mut point = self.grid.cursor.point;
            point.line += self.grid.display_offset();
            point
        };

        // Cursor shape.
        let hidden =
            !self.mode.contains(TermMode::SHOW_CURSOR) || point.line >= self.screen_lines();
        let cursor_style = if hidden && !vi_mode {
            point.line = Line(0);
            CursorStyle::Hidden
        } else if !self.is_focused && config.cursor.unfocused_hollow() {
            CursorStyle::HollowBlock
        } else {
            let cursor_style = self.cursor_style.unwrap_or(self.default_cursor_style);

            if vi_mode {
                self.vi_mode_cursor_style.unwrap_or(cursor_style)
            } else {
                cursor_style
            }
        };

        // Cursor colors.
        let color = if vi_mode { config.colors.vi_mode_cursor } else { config.colors.cursor };
        let cursor_color = if self.color_modified[NamedColor::Cursor as usize] {
            CellRgb::Rgb(self.colors[NamedColor::Cursor])
        } else {
            color.cursor()
        };
        let text_color = color.text();

        // Expand across wide cell when inside wide char or spacer.
        let buffer_point = self.visible_to_buffer(point);
        let cell = self.grid[buffer_point.line][buffer_point.col];
        let is_wide = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            point.col -= 1;
            true
        } else {
            cell.flags.contains(Flags::WIDE_CHAR)
        };

        RenderableCursor {
            text_color,
            cursor_color,
            key: CursorKey { style: cursor_style, is_wide },
            point,
            rendered: false,
        }
    }
}

impl<T> Dimensions for Term<T> {
    #[inline]
    fn cols(&self) -> Column {
        self.grid.cols()
    }

    #[inline]
    fn screen_lines(&self) -> Line {
        self.grid.screen_lines()
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.grid.total_lines()
    }
}

impl<T: EventListener> Handler for Term<T> {
    /// A character to be displayed.
    #[inline]
    fn input(&mut self, c: char) {
        // Number of cells the char will occupy.
        let width = match c.width() {
            Some(width) => width,
            None => return,
        };

        // Handle zero-width characters.
        if width == 0 {
            let mut col = self.grid.cursor.point.col.0.saturating_sub(1);
            let line = self.grid.cursor.point.line;
            if self.grid[line][Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
                col = col.saturating_sub(1);
            }
            self.grid[line][Column(col)].push_extra(c);
            return;
        }

        // Move cursor to next line.
        if self.grid.cursor.input_needs_wrap {
            self.wrapline();
        }

        let num_cols = self.cols();

        // If in insert mode, first shift cells to the right.
        if self.mode.contains(TermMode::INSERT) && self.grid.cursor.point.col + width < num_cols {
            let line = self.grid.cursor.point.line;
            let col = self.grid.cursor.point.col;
            let line = &mut self.grid[line];

            let src = line[col..].as_ptr();
            let dst = line[(col + width)..].as_mut_ptr();
            unsafe {
                ptr::copy(src, dst, (num_cols - col - width).0);
            }
        }

        if width == 1 {
            self.write_at_cursor(c);
        } else {
            if self.grid.cursor.point.col + 1 >= num_cols {
                if self.mode.contains(TermMode::LINE_WRAP) {
                    // Insert placeholder before wide char if glyph does not fit in this row.
                    self.write_at_cursor(' ').flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);
                    self.wrapline();
                } else {
                    // Prevent out of bounds crash when linewrapping is disabled.
                    self.grid.cursor.input_needs_wrap = true;
                    return;
                }
            }

            // Write full width glyph to current cursor cell.
            self.write_at_cursor(c).flags.insert(Flags::WIDE_CHAR);

            // Write spacer to cell following the wide glyph.
            self.grid.cursor.point.col += 1;
            self.write_at_cursor(' ').flags.insert(Flags::WIDE_CHAR_SPACER);
        }

        if self.grid.cursor.point.col + 1 < num_cols {
            self.grid.cursor.point.col += 1;
        } else {
            self.grid.cursor.input_needs_wrap = true;
        }
    }

    #[inline]
    fn decaln(&mut self) {
        trace!("Decalnning");

        let template = Cell { c: 'E', ..Cell::default() };
        self.grid.region_mut(..).each(|c| c.reset(&template));
    }

    #[inline]
    fn goto(&mut self, line: Line, col: Column) {
        trace!("Going to: line={}, col={}", line, col);
        let (y_offset, max_y) = if self.mode.contains(TermMode::ORIGIN) {
            (self.scroll_region.start, self.scroll_region.end - 1)
        } else {
            (Line(0), self.screen_lines() - 1)
        };

        self.grid.cursor.point.line = min(line + y_offset, max_y);
        self.grid.cursor.point.col = min(col, self.cols() - 1);
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn goto_line(&mut self, line: Line) {
        trace!("Going to line: {}", line);
        self.goto(line, self.grid.cursor.point.col)
    }

    #[inline]
    fn goto_col(&mut self, col: Column) {
        trace!("Going to column: {}", col);
        self.goto(self.grid.cursor.point.line, col)
    }

    #[inline]
    fn insert_blank(&mut self, count: Column) {
        let cursor = self.grid.cursor;

        // Ensure inserting within terminal bounds
        let count = min(count, self.cols() - cursor.point.col);

        let source = cursor.point.col;
        let destination = cursor.point.col + count;
        let num_cells = (self.cols() - destination).0;

        let line = &mut self.grid[cursor.point.line];

        unsafe {
            let src = line[source..].as_ptr();
            let dst = line[destination..].as_mut_ptr();

            ptr::copy(src, dst, num_cells);
        }

        // Cells were just moved out toward the end of the line;
        // fill in between source and dest with blanks.
        for c in &mut line[source..destination] {
            c.reset(&cursor.template);
        }
    }

    #[inline]
    fn move_up(&mut self, lines: Line) {
        trace!("Moving up: {}", lines);
        let move_to = Line(self.grid.cursor.point.line.0.saturating_sub(lines.0));
        self.goto(move_to, self.grid.cursor.point.col)
    }

    #[inline]
    fn move_down(&mut self, lines: Line) {
        trace!("Moving down: {}", lines);
        let move_to = self.grid.cursor.point.line + lines;
        self.goto(move_to, self.grid.cursor.point.col)
    }

    #[inline]
    fn move_forward(&mut self, cols: Column) {
        trace!("Moving forward: {}", cols);
        let num_cols = self.cols();
        self.grid.cursor.point.col = min(self.grid.cursor.point.col + cols, num_cols - 1);
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn move_backward(&mut self, cols: Column) {
        trace!("Moving backward: {}", cols);
        self.grid.cursor.point.col = Column(self.grid.cursor.point.col.saturating_sub(cols.0));
        self.grid.cursor.input_needs_wrap = false;
    }

    #[inline]
    fn identify_terminal<W: io::Write>(&mut self, writer: &mut W, intermediate: Option<char>) {
        match intermediate {
            None => {
                trace!("Reporting primary device attributes");
                let _ = writer.write_all(b"\x1b[?6c");
            },
            Some('>') => {
                trace!("Reporting secondary device attributes");
                let version = version_number(env!("CARGO_PKG_VERSION"));
                let _ = writer.write_all(format!("\x1b[>0;{};1c", version).as_bytes());
            },
            _ => debug!("Unsupported device attributes intermediate"),
        }
    }

    #[inline]
    fn device_status<W: io::Write>(&mut self, writer: &mut W, arg: usize) {
        trace!("Reporting device status: {}", arg);
        match arg {
            5 => {
                let _ = writer.write_all(b"\x1b[0n");
            },
            6 => {
                let pos = self.grid.cursor.point;
                let response = format!("\x1b[{};{}R", pos.line + 1, pos.col + 1);
                let _ = writer.write_all(response.as_bytes());
            },
            _ => debug!("unknown device status query: {}", arg),
        };
    }

    #[inline]
    fn move_down_and_cr(&mut self, lines: Line) {
        trace!("Moving down and cr: {}", lines);
        let move_to = self.grid.cursor.point.line + lines;
        self.goto(move_to, Column(0))
    }

    #[inline]
    fn move_up_and_cr(&mut self, lines: Line) {
        trace!("Moving up and cr: {}", lines);
        let move_to = Line(self.grid.cursor.point.line.0.saturating_sub(lines.0));
        self.goto(move_to, Column(0))
    }

    /// Insert tab at cursor position.
    #[inline]
    fn put_tab(&mut self, mut count: i64) {
        // A tab after the last column is the same as a linebreak.
        if self.grid.cursor.input_needs_wrap {
            self.wrapline();
            return;
        }

        while self.grid.cursor.point.col < self.cols() && count != 0 {
            count -= 1;

            let c = self.grid.cursor.charsets[self.active_charset].map('\t');
            let cell = self.grid.cursor_cell();
            if cell.c == ' ' {
                cell.c = c;
            }

            loop {
                if (self.grid.cursor.point.col + 1) == self.cols() {
                    break;
                }

                self.grid.cursor.point.col += 1;

                if self.tabs[self.grid.cursor.point.col] {
                    break;
                }
            }
        }
    }

    /// Backspace `count` characters.
    #[inline]
    fn backspace(&mut self) {
        trace!("Backspace");

        if self.grid.cursor.point.col > Column(0) {
            self.grid.cursor.point.col -= 1;
            self.grid.cursor.input_needs_wrap = false;
        }
    }

    /// Carriage return.
    #[inline]
    fn carriage_return(&mut self) {
        trace!("Carriage return");
        self.grid.cursor.point.col = Column(0);
        self.grid.cursor.input_needs_wrap = false;
    }

    /// Linefeed.
    #[inline]
    fn linefeed(&mut self) {
        trace!("Linefeed");
        let next = self.grid.cursor.point.line + 1;
        if next == self.scroll_region.end {
            self.scroll_up(Line(1));
        } else if next < self.screen_lines() {
            self.grid.cursor.point.line += 1;
        }
    }

    /// Set current position as a tabstop.
    #[inline]
    fn bell(&mut self) {
        trace!("Bell");
        self.visual_bell.ring();
        self.event_proxy.send_event(Event::Bell);
    }

    #[inline]
    fn substitute(&mut self) {
        trace!("[unimplemented] Substitute");
    }

    /// Run LF/NL.
    ///
    /// LF/NL mode has some interesting history. According to ECMA-48 4th
    /// edition, in LINE FEED mode,
    ///
    /// > The execution of the formatter functions LINE FEED (LF), FORM FEED
    /// (FF), LINE TABULATION (VT) cause only movement of the active position in
    /// the direction of the line progression.
    ///
    /// In NEW LINE mode,
    ///
    /// > The execution of the formatter functions LINE FEED (LF), FORM FEED
    /// (FF), LINE TABULATION (VT) cause movement to the line home position on
    /// the following line, the following form, etc. In the case of LF this is
    /// referred to as the New Line (NL) option.
    ///
    /// Additionally, ECMA-48 4th edition says that this option is deprecated.
    /// ECMA-48 5th edition only mentions this option (without explanation)
    /// saying that it's been removed.
    ///
    /// As an emulator, we need to support it since applications may still rely
    /// on it.
    #[inline]
    fn newline(&mut self) {
        self.linefeed();

        if self.mode.contains(TermMode::LINE_FEED_NEW_LINE) {
            self.carriage_return();
        }
    }

    #[inline]
    fn set_horizontal_tabstop(&mut self) {
        trace!("Setting horizontal tabstop");
        self.tabs[self.grid.cursor.point.col] = true;
    }

    #[inline]
    fn scroll_up(&mut self, lines: Line) {
        let origin = self.scroll_region.start;
        self.scroll_up_relative(origin, lines);
    }

    #[inline]
    fn scroll_down(&mut self, lines: Line) {
        let origin = self.scroll_region.start;
        self.scroll_down_relative(origin, lines);
    }

    #[inline]
    fn insert_blank_lines(&mut self, lines: Line) {
        trace!("Inserting blank {} lines", lines);

        let origin = self.grid.cursor.point.line;
        if self.scroll_region.contains(&origin) {
            self.scroll_down_relative(origin, lines);
        }
    }

    #[inline]
    fn delete_lines(&mut self, lines: Line) {
        let origin = self.grid.cursor.point.line;
        let lines = min(self.screen_lines() - origin, lines);

        trace!("Deleting {} lines", lines);

        if lines.0 > 0 && self.scroll_region.contains(&self.grid.cursor.point.line) {
            self.scroll_up_relative(origin, lines);
        }
    }

    #[inline]
    fn erase_chars(&mut self, count: Column) {
        let cursor = self.grid.cursor;

        trace!("Erasing chars: count={}, col={}", count, cursor.point.col);

        let start = cursor.point.col;
        let end = min(start + count, self.cols());

        // Cleared cells have current background color set.
        let row = &mut self.grid[cursor.point.line];
        for c in &mut row[start..end] {
            c.reset(&cursor.template);
        }
    }

    #[inline]
    fn delete_chars(&mut self, count: Column) {
        let cols = self.cols();
        let cursor = self.grid.cursor;

        // Ensure deleting within terminal bounds.
        let count = min(count, cols);

        let start = cursor.point.col;
        let end = min(start + count, cols - 1);
        let n = (cols - end).0;

        let line = &mut self.grid[cursor.point.line];

        unsafe {
            let src = line[end..].as_ptr();
            let dst = line[start..].as_mut_ptr();

            ptr::copy(src, dst, n);
        }

        // Clear last `count` cells in line. If deleting 1 char, need to delete
        // 1 cell.
        let end = cols - count;
        for c in &mut line[end..] {
            c.reset(&cursor.template);
        }
    }

    #[inline]
    fn move_backward_tabs(&mut self, count: i64) {
        trace!("Moving backward {} tabs", count);

        for _ in 0..count {
            let mut col = self.grid.cursor.point.col;
            for i in (0..(col.0)).rev() {
                if self.tabs[index::Column(i)] {
                    col = index::Column(i);
                    break;
                }
            }
            self.grid.cursor.point.col = col;
        }
    }

    #[inline]
    fn move_forward_tabs(&mut self, count: i64) {
        trace!("[unimplemented] Moving forward {} tabs", count);
    }

    #[inline]
    fn save_cursor_position(&mut self) {
        trace!("Saving cursor position");

        self.grid.saved_cursor = self.grid.cursor;
    }

    #[inline]
    fn restore_cursor_position(&mut self) {
        trace!("Restoring cursor position");

        self.grid.cursor = self.grid.saved_cursor;
    }

    #[inline]
    fn clear_line(&mut self, mode: ansi::LineClearMode) {
        trace!("Clearing line: {:?}", mode);

        let cursor = self.grid.cursor;
        match mode {
            ansi::LineClearMode::Right => {
                let row = &mut self.grid[cursor.point.line];
                for cell in &mut row[cursor.point.col..] {
                    cell.reset(&cursor.template);
                }
            },
            ansi::LineClearMode::Left => {
                let row = &mut self.grid[cursor.point.line];
                for cell in &mut row[..=cursor.point.col] {
                    cell.reset(&cursor.template);
                }
            },
            ansi::LineClearMode::All => {
                let row = &mut self.grid[cursor.point.line];
                for cell in &mut row[..] {
                    cell.reset(&cursor.template);
                }
            },
        }

        let cursor_buffer_line = (self.screen_lines() - self.grid.cursor.point.line - 1).0;
        self.selection = self
            .selection
            .take()
            .filter(|s| !s.intersects_range(cursor_buffer_line..=cursor_buffer_line));
    }

    /// Set the indexed color value.
    #[inline]
    fn set_color(&mut self, index: usize, color: Rgb) {
        trace!("Setting color[{}] = {:?}", index, color);
        self.colors[index] = color;
        self.color_modified[index] = true;
    }

    /// Write a foreground/background color escape sequence with the current color.
    #[inline]
    fn dynamic_color_sequence<W: io::Write>(
        &mut self,
        writer: &mut W,
        code: u8,
        index: usize,
        terminator: &str,
    ) {
        trace!("Writing escape sequence for dynamic color code {}: color[{}]", code, index);
        let color = self.colors[index];
        let response = format!(
            "\x1b]{};rgb:{1:02x}{1:02x}/{2:02x}{2:02x}/{3:02x}{3:02x}{4}",
            code, color.r, color.g, color.b, terminator
        );
        let _ = writer.write_all(response.as_bytes());
    }

    /// Reset the indexed color to original value.
    #[inline]
    fn reset_color(&mut self, index: usize) {
        trace!("Resetting color[{}]", index);
        self.colors[index] = self.original_colors[index];
        self.color_modified[index] = false;
    }

    /// Store data into clipboard.
    #[inline]
    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        if let Ok(bytes) = base64::decode(base64) {
            if let Ok(text) = String::from_utf8(bytes) {
                self.event_proxy.send_event(Event::ClipboardStore(clipboard_type, text));
            }
        }
    }

    /// Load data from clipboard.
    #[inline]
    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        let clipboard_type = match clipboard {
            b'c' => ClipboardType::Clipboard,
            b'p' | b's' => ClipboardType::Selection,
            _ => return,
        };

        let terminator = terminator.to_owned();

        self.event_proxy.send_event(Event::ClipboardLoad(
            clipboard_type,
            Arc::new(move |text| {
                let base64 = base64::encode(&text);
                format!("\x1b]52;{};{}{}", clipboard as char, base64, terminator)
            }),
        ));
    }

    #[inline]
    fn clear_screen(&mut self, mode: ansi::ClearMode) {
        trace!("Clearing screen: {:?}", mode);
        let template = self.grid.cursor.template;

        let num_lines = self.screen_lines().0;
        let cursor_buffer_line = num_lines - self.grid.cursor.point.line.0 - 1;

        match mode {
            ansi::ClearMode::Above => {
                let cursor = self.grid.cursor.point;

                // If clearing more than one line.
                if cursor.line > Line(1) {
                    // Fully clear all lines before the current line.
                    self.grid.region_mut(..cursor.line).each(|cell| cell.reset(&template));
                }

                // Clear up to the current column in the current line.
                let end = min(cursor.col + 1, self.cols());
                for cell in &mut self.grid[cursor.line][..end] {
                    cell.reset(&template);
                }

                self.selection = self
                    .selection
                    .take()
                    .filter(|s| !s.intersects_range(cursor_buffer_line..num_lines));
            },
            ansi::ClearMode::Below => {
                let cursor = self.grid.cursor.point;
                for cell in &mut self.grid[cursor.line][cursor.col..] {
                    cell.reset(&template);
                }

                if cursor.line.0 < num_lines - 1 {
                    self.grid.region_mut((cursor.line + 1)..).each(|cell| cell.reset(&template));
                }

                self.selection =
                    self.selection.take().filter(|s| !s.intersects_range(..=cursor_buffer_line));
            },
            ansi::ClearMode::All => {
                if self.mode.contains(TermMode::ALT_SCREEN) {
                    self.grid.region_mut(..).each(|c| c.reset(&template));
                } else {
                    let template = Cell { bg: template.bg, ..Cell::default() };
                    self.grid.clear_viewport(template);
                }

                self.selection = self.selection.take().filter(|s| !s.intersects_range(..num_lines));
            },
            ansi::ClearMode::Saved if self.history_size() > 0 => {
                self.grid.clear_history();

                self.selection = self.selection.take().filter(|s| !s.intersects_range(num_lines..));
            },
            // We have no history to clear.
            ansi::ClearMode::Saved => (),
        }
    }

    #[inline]
    fn clear_tabs(&mut self, mode: ansi::TabulationClearMode) {
        trace!("Clearing tabs: {:?}", mode);
        match mode {
            ansi::TabulationClearMode::Current => {
                self.tabs[self.grid.cursor.point.col] = false;
            },
            ansi::TabulationClearMode::All => {
                self.tabs.clear_all();
            },
        }
    }

    /// Reset all important fields in the term struct.
    #[inline]
    fn reset_state(&mut self) {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            mem::swap(&mut self.grid, &mut self.inactive_grid);
        }
        self.active_charset = Default::default();
        self.mode = Default::default();
        self.colors = self.original_colors;
        self.color_modified = [false; color::COUNT];
        self.cursor_style = None;
        self.grid.reset(Cell::default());
        self.inactive_grid.reset(Cell::default());
        self.scroll_region = Line(0)..self.screen_lines();
        self.tabs = TabStops::new(self.cols());
        self.title_stack = Vec::new();
        self.title = None;
        self.selection = None;
        self.regex_search = None;
    }

    #[inline]
    fn reverse_index(&mut self) {
        trace!("Reversing index");
        // If cursor is at the top.
        if self.grid.cursor.point.line == self.scroll_region.start {
            self.scroll_down(Line(1));
        } else {
            self.grid.cursor.point.line = Line(self.grid.cursor.point.line.saturating_sub(1));
        }
    }

    /// Set a terminal attribute.
    #[inline]
    fn terminal_attribute(&mut self, attr: Attr) {
        trace!("Setting attribute: {:?}", attr);
        let cursor = &mut self.grid.cursor;
        match attr {
            Attr::Foreground(color) => cursor.template.fg = color,
            Attr::Background(color) => cursor.template.bg = color,
            Attr::Reset => {
                cursor.template.fg = Color::Named(NamedColor::Foreground);
                cursor.template.bg = Color::Named(NamedColor::Background);
                cursor.template.flags = Flags::empty();
            },
            Attr::Reverse => cursor.template.flags.insert(Flags::INVERSE),
            Attr::CancelReverse => cursor.template.flags.remove(Flags::INVERSE),
            Attr::Bold => cursor.template.flags.insert(Flags::BOLD),
            Attr::CancelBold => cursor.template.flags.remove(Flags::BOLD),
            Attr::Dim => cursor.template.flags.insert(Flags::DIM),
            Attr::CancelBoldDim => cursor.template.flags.remove(Flags::BOLD | Flags::DIM),
            Attr::Italic => cursor.template.flags.insert(Flags::ITALIC),
            Attr::CancelItalic => cursor.template.flags.remove(Flags::ITALIC),
            Attr::Underline => {
                cursor.template.flags.remove(Flags::DOUBLE_UNDERLINE);
                cursor.template.flags.insert(Flags::UNDERLINE);
            },
            Attr::DoubleUnderline => {
                cursor.template.flags.remove(Flags::UNDERLINE);
                cursor.template.flags.insert(Flags::DOUBLE_UNDERLINE);
            },
            Attr::CancelUnderline => {
                cursor.template.flags.remove(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE);
            },
            Attr::Hidden => cursor.template.flags.insert(Flags::HIDDEN),
            Attr::CancelHidden => cursor.template.flags.remove(Flags::HIDDEN),
            Attr::Strike => cursor.template.flags.insert(Flags::STRIKEOUT),
            Attr::CancelStrike => cursor.template.flags.remove(Flags::STRIKEOUT),
            _ => {
                debug!("Term got unhandled attr: {:?}", attr);
            },
        }
    }

    #[inline]
    fn set_mode(&mut self, mode: ansi::Mode) {
        trace!("Setting mode: {:?}", mode);
        match mode {
            ansi::Mode::UrgencyHints => self.mode.insert(TermMode::URGENCY_HINTS),
            ansi::Mode::SwapScreenAndSetRestoreCursor => {
                if !self.mode.contains(TermMode::ALT_SCREEN) {
                    self.swap_alt();
                }
            },
            ansi::Mode::ShowCursor => self.mode.insert(TermMode::SHOW_CURSOR),
            ansi::Mode::CursorKeys => self.mode.insert(TermMode::APP_CURSOR),
            // Mouse protocols are mutually exclusive.
            ansi::Mode::ReportMouseClicks => {
                self.mode.remove(TermMode::MOUSE_MODE);
                self.mode.insert(TermMode::MOUSE_REPORT_CLICK);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            },
            ansi::Mode::ReportCellMouseMotion => {
                self.mode.remove(TermMode::MOUSE_MODE);
                self.mode.insert(TermMode::MOUSE_DRAG);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            },
            ansi::Mode::ReportAllMouseMotion => {
                self.mode.remove(TermMode::MOUSE_MODE);
                self.mode.insert(TermMode::MOUSE_MOTION);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            },
            ansi::Mode::ReportFocusInOut => self.mode.insert(TermMode::FOCUS_IN_OUT),
            ansi::Mode::BracketedPaste => self.mode.insert(TermMode::BRACKETED_PASTE),
            // Mouse encodings are mutually exclusive.
            ansi::Mode::SgrMouse => {
                self.mode.remove(TermMode::UTF8_MOUSE);
                self.mode.insert(TermMode::SGR_MOUSE);
            },
            ansi::Mode::Utf8Mouse => {
                self.mode.remove(TermMode::SGR_MOUSE);
                self.mode.insert(TermMode::UTF8_MOUSE);
            },
            ansi::Mode::AlternateScroll => self.mode.insert(TermMode::ALTERNATE_SCROLL),
            ansi::Mode::LineWrap => self.mode.insert(TermMode::LINE_WRAP),
            ansi::Mode::LineFeedNewLine => self.mode.insert(TermMode::LINE_FEED_NEW_LINE),
            ansi::Mode::Origin => self.mode.insert(TermMode::ORIGIN),
            ansi::Mode::DECCOLM => self.deccolm(),
            ansi::Mode::Insert => self.mode.insert(TermMode::INSERT),
            ansi::Mode::BlinkingCursor => {
                trace!("... unimplemented mode");
            },
        }
    }

    #[inline]
    fn unset_mode(&mut self, mode: ansi::Mode) {
        trace!("Unsetting mode: {:?}", mode);
        match mode {
            ansi::Mode::UrgencyHints => self.mode.remove(TermMode::URGENCY_HINTS),
            ansi::Mode::SwapScreenAndSetRestoreCursor => {
                if self.mode.contains(TermMode::ALT_SCREEN) {
                    self.swap_alt();
                }
            },
            ansi::Mode::ShowCursor => self.mode.remove(TermMode::SHOW_CURSOR),
            ansi::Mode::CursorKeys => self.mode.remove(TermMode::APP_CURSOR),
            ansi::Mode::ReportMouseClicks => {
                self.mode.remove(TermMode::MOUSE_REPORT_CLICK);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            },
            ansi::Mode::ReportCellMouseMotion => {
                self.mode.remove(TermMode::MOUSE_DRAG);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            },
            ansi::Mode::ReportAllMouseMotion => {
                self.mode.remove(TermMode::MOUSE_MOTION);
                self.event_proxy.send_event(Event::MouseCursorDirty);
            },
            ansi::Mode::ReportFocusInOut => self.mode.remove(TermMode::FOCUS_IN_OUT),
            ansi::Mode::BracketedPaste => self.mode.remove(TermMode::BRACKETED_PASTE),
            ansi::Mode::SgrMouse => self.mode.remove(TermMode::SGR_MOUSE),
            ansi::Mode::Utf8Mouse => self.mode.remove(TermMode::UTF8_MOUSE),
            ansi::Mode::AlternateScroll => self.mode.remove(TermMode::ALTERNATE_SCROLL),
            ansi::Mode::LineWrap => self.mode.remove(TermMode::LINE_WRAP),
            ansi::Mode::LineFeedNewLine => self.mode.remove(TermMode::LINE_FEED_NEW_LINE),
            ansi::Mode::Origin => self.mode.remove(TermMode::ORIGIN),
            ansi::Mode::DECCOLM => self.deccolm(),
            ansi::Mode::Insert => self.mode.remove(TermMode::INSERT),
            ansi::Mode::BlinkingCursor => {
                trace!("... unimplemented mode");
            },
        }
    }

    #[inline]
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        // Fallback to the last line as default.
        let bottom = bottom.unwrap_or_else(|| self.screen_lines().0);

        if top >= bottom {
            debug!("Invalid scrolling region: ({};{})", top, bottom);
            return;
        }

        // Bottom should be included in the range, but range end is not
        // usually included. One option would be to use an inclusive
        // range, but instead we just let the open range end be 1
        // higher.
        let start = Line(top - 1);
        let end = Line(bottom);

        trace!("Setting scrolling region: ({};{})", start, end);

        self.scroll_region.start = min(start, self.screen_lines());
        self.scroll_region.end = min(end, self.screen_lines());
        self.goto(Line(0), Column(0));
    }

    #[inline]
    fn set_keypad_application_mode(&mut self) {
        trace!("Setting keypad application mode");
        self.mode.insert(TermMode::APP_KEYPAD);
    }

    #[inline]
    fn unset_keypad_application_mode(&mut self) {
        trace!("Unsetting keypad application mode");
        self.mode.remove(TermMode::APP_KEYPAD);
    }

    #[inline]
    fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        trace!("Configuring charset {:?} as {:?}", index, charset);
        self.grid.cursor.charsets[index] = charset;
    }

    #[inline]
    fn set_active_charset(&mut self, index: CharsetIndex) {
        trace!("Setting active charset {:?}", index);
        self.active_charset = index;
    }

    #[inline]
    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        trace!("Setting cursor style {:?}", style);
        self.cursor_style = style;
    }

    #[inline]
    fn set_title(&mut self, title: Option<String>) {
        trace!("Setting title to '{:?}'", title);

        self.title = title.clone();

        let title_event = match title {
            Some(title) => Event::Title(title),
            None => Event::ResetTitle,
        };

        self.event_proxy.send_event(title_event);
    }

    #[inline]
    fn push_title(&mut self) {
        trace!("Pushing '{:?}' onto title stack", self.title);

        if self.title_stack.len() >= TITLE_STACK_MAX_DEPTH {
            let removed = self.title_stack.remove(0);
            trace!(
                "Removing '{:?}' from bottom of title stack that exceeds its maximum depth",
                removed
            );
        }

        self.title_stack.push(self.title.clone());
    }

    #[inline]
    fn pop_title(&mut self) {
        trace!("Attempting to pop title from stack...");

        if let Some(popped) = self.title_stack.pop() {
            trace!("Title '{:?}' popped from stack", popped);
            self.set_title(popped);
        }
    }

    #[inline]
    fn text_area_size_pixels<W: io::Write>(&mut self, writer: &mut W) {
        let width = self.cell_width * self.cols().0;
        let height = self.cell_height * self.screen_lines().0;
        let _ = write!(writer, "\x1b[4;{};{}t", height, width);
    }

    #[inline]
    fn text_area_size_chars<W: io::Write>(&mut self, writer: &mut W) {
        let _ = write!(writer, "\x1b[8;{};{}t", self.screen_lines(), self.cols());
    }
}

/// Terminal version for escape sequence reports.
///
/// This returns the current terminal version as a unique number based on alacritty_terminal's
/// semver version. The different versions are padded to ensure that a higher semver version will
/// always report a higher version number.
fn version_number(mut version: &str) -> usize {
    if let Some(separator) = version.rfind('-') {
        version = &version[..separator];
    }

    let mut version_number = 0;

    let semver_versions = version.split('.');
    for (i, semver_version) in semver_versions.rev().enumerate() {
        let semver_number = semver_version.parse::<usize>().unwrap_or(0);
        version_number += usize::pow(100, i as u32) * semver_number;
    }

    version_number
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardType {
    Clipboard,
    Selection,
}

struct TabStops {
    tabs: Vec<bool>,
}

impl TabStops {
    #[inline]
    fn new(num_cols: Column) -> TabStops {
        TabStops {
            tabs: IndexRange::from(Column(0)..num_cols)
                .map(|i| (*i as usize) % INITIAL_TABSTOPS == 0)
                .collect::<Vec<bool>>(),
        }
    }

    /// Remove all tabstops.
    #[inline]
    fn clear_all(&mut self) {
        unsafe {
            ptr::write_bytes(self.tabs.as_mut_ptr(), 0, self.tabs.len());
        }
    }

    /// Increase tabstop capacity.
    #[inline]
    fn resize(&mut self, num_cols: Column) {
        let mut index = self.tabs.len();
        self.tabs.resize_with(num_cols.0, || {
            let is_tabstop = index % INITIAL_TABSTOPS == 0;
            index += 1;
            is_tabstop
        });
    }
}

impl Index<Column> for TabStops {
    type Output = bool;

    fn index(&self, index: Column) -> &bool {
        &self.tabs[index.0]
    }
}

impl IndexMut<Column> for TabStops {
    fn index_mut(&mut self, index: Column) -> &mut bool {
        self.tabs.index_mut(index.0)
    }
}

/// Terminal test helpers.
pub mod test {
    use super::*;

    use unicode_width::UnicodeWidthChar;

    use crate::config::Config;
    use crate::index::Column;

    /// Construct a terminal from its content as string.
    ///
    /// A `\n` will break line and `\r\n` will break line without wrapping.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use alacritty_terminal::term::test::mock_term;
    ///
    /// // Create a terminal with the following cells:
    /// //
    /// // [h][e][l][l][o] <- WRAPLINE flag set
    /// // [:][)][ ][ ][ ]
    /// // [t][e][s][t][ ]
    /// mock_term(
    ///     "\
    ///     hello\n:)\r\ntest",
    /// );
    /// ```
    pub fn mock_term(content: &str) -> Term<()> {
        let lines: Vec<&str> = content.split('\n').collect();
        let num_cols = lines
            .iter()
            .map(|line| line.chars().filter(|c| *c != '\r').map(|c| c.width().unwrap()).sum())
            .max()
            .unwrap_or(0);

        // Create terminal with the appropriate dimensions.
        let size = SizeInfo::new(num_cols as f32, lines.len() as f32, 1., 1., 0., 0., false);
        let mut term = Term::new(&Config::<()>::default(), size, ());

        // Fill terminal with content.
        for (line, text) in lines.iter().rev().enumerate() {
            if !text.ends_with('\r') && line != 0 {
                term.grid[line][Column(num_cols - 1)].flags.insert(Flags::WRAPLINE);
            }

            let mut index = 0;
            for c in text.chars().take_while(|c| *c != '\r') {
                term.grid[line][Column(index)].c = c;

                // Handle fullwidth characters.
                let width = c.width().unwrap();
                if width == 2 {
                    term.grid[line][Column(index)].flags.insert(Flags::WIDE_CHAR);
                    term.grid[line][Column(index + 1)].flags.insert(Flags::WIDE_CHAR_SPACER);
                }

                index += width;
            }
        }

        term
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::mem;

    use crate::ansi::{self, CharsetIndex, Handler, StandardCharset};
    use crate::config::MockConfig;
    use crate::event::{Event, EventListener};
    use crate::grid::{Grid, Scroll};
    use crate::index::{Column, Line, Point, Side};
    use crate::selection::{Selection, SelectionType};
    use crate::term::cell::{Cell, Flags};

    struct Mock;
    impl EventListener for Mock {
        fn send_event(&self, _event: Event) {}
    }

    #[test]
    fn semantic_selection_works() {
        let size = SizeInfo::new(21.0, 51.0, 3.0, 3.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);
        let mut grid: Grid<Cell> = Grid::new(Line(3), Column(5), 0, Cell::default());
        for i in 0..5 {
            for j in 0..2 {
                grid[Line(j)][Column(i)].c = 'a';
            }
        }
        grid[Line(0)][Column(0)].c = '"';
        grid[Line(0)][Column(3)].c = '"';
        grid[Line(1)][Column(2)].c = '"';
        grid[Line(0)][Column(4)].flags.insert(Flags::WRAPLINE);

        let mut escape_chars = String::from("\"");

        mem::swap(&mut term.grid, &mut grid);
        mem::swap(&mut term.semantic_escape_chars, &mut escape_chars);

        {
            term.selection = Some(Selection::new(
                SelectionType::Semantic,
                Point { line: 2, col: Column(1) },
                Side::Left,
            ));
            assert_eq!(term.selection_to_string(), Some(String::from("aa")));
        }

        {
            term.selection = Some(Selection::new(
                SelectionType::Semantic,
                Point { line: 2, col: Column(4) },
                Side::Left,
            ));
            assert_eq!(term.selection_to_string(), Some(String::from("aaa")));
        }

        {
            term.selection = Some(Selection::new(
                SelectionType::Semantic,
                Point { line: 1, col: Column(1) },
                Side::Left,
            ));
            assert_eq!(term.selection_to_string(), Some(String::from("aaa")));
        }
    }

    #[test]
    fn line_selection_works() {
        let size = SizeInfo::new(21.0, 51.0, 3.0, 3.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);
        let mut grid: Grid<Cell> = Grid::new(Line(1), Column(5), 0, Cell::default());
        for i in 0..5 {
            grid[Line(0)][Column(i)].c = 'a';
        }
        grid[Line(0)][Column(0)].c = '"';
        grid[Line(0)][Column(3)].c = '"';

        mem::swap(&mut term.grid, &mut grid);

        term.selection = Some(Selection::new(
            SelectionType::Lines,
            Point { line: 0, col: Column(3) },
            Side::Left,
        ));
        assert_eq!(term.selection_to_string(), Some(String::from("\"aa\"a\n")));
    }

    #[test]
    fn selecting_empty_line() {
        let size = SizeInfo::new(21.0, 51.0, 3.0, 3.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);
        let mut grid: Grid<Cell> = Grid::new(Line(3), Column(3), 0, Cell::default());
        for l in 0..3 {
            if l != 1 {
                for c in 0..3 {
                    grid[Line(l)][Column(c)].c = 'a';
                }
            }
        }

        mem::swap(&mut term.grid, &mut grid);

        let mut selection =
            Selection::new(SelectionType::Simple, Point { line: 2, col: Column(0) }, Side::Left);
        selection.update(Point { line: 0, col: Column(2) }, Side::Right);
        term.selection = Some(selection);
        assert_eq!(term.selection_to_string(), Some("aaa\n\naaa\n".into()));
    }

    /// Check that the grid can be serialized back and forth losslessly.
    ///
    /// This test is in the term module as opposed to the grid since we want to
    /// test this property with a T=Cell.
    #[test]
    fn grid_serde() {
        let template = Cell::default();

        let grid: Grid<Cell> = Grid::new(Line(24), Column(80), 0, template);
        let serialized = serde_json::to_string(&grid).expect("ser");
        let deserialized = serde_json::from_str::<Grid<Cell>>(&serialized).expect("de");

        assert_eq!(deserialized, grid);
    }

    #[test]
    fn input_line_drawing_character() {
        let size = SizeInfo::new(21.0, 51.0, 3.0, 3.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);
        let cursor = Point::new(Line(0), Column(0));
        term.configure_charset(CharsetIndex::G0, StandardCharset::SpecialCharacterAndLineDrawing);
        term.input('a');

        assert_eq!(term.grid()[&cursor].c, '▒');
    }

    #[test]
    fn clear_saved_lines() {
        let size = SizeInfo::new(21.0, 51.0, 3.0, 3.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);

        // Add one line of scrollback.
        term.grid.scroll_up(&(Line(0)..Line(1)), Line(1), Cell::default());

        // Clear the history.
        term.clear_screen(ansi::ClearMode::Saved);

        // Make sure that scrolling does not change the grid.
        let mut scrolled_grid = term.grid.clone();
        scrolled_grid.scroll_display(Scroll::Top);

        // Truncate grids for comparison.
        scrolled_grid.truncate();
        term.grid.truncate();

        assert_eq!(term.grid, scrolled_grid);
    }

    #[test]
    fn grow_lines_updates_active_cursor_pos() {
        let mut size = SizeInfo::new(100.0, 10.0, 1.0, 1.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Increase visible lines.
        size.screen_lines.0 = 30;
        term.resize(size);

        assert_eq!(term.history_size(), 0);
        assert_eq!(term.grid.cursor.point, Point::new(Line(19), Column(0)));
    }

    #[test]
    fn grow_lines_updates_inactive_cursor_pos() {
        let mut size = SizeInfo::new(100.0, 10.0, 1.0, 1.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Enter alt screen.
        term.set_mode(ansi::Mode::SwapScreenAndSetRestoreCursor);

        // Increase visible lines.
        size.screen_lines.0 = 30;
        term.resize(size);

        // Leave alt screen.
        term.unset_mode(ansi::Mode::SwapScreenAndSetRestoreCursor);

        assert_eq!(term.history_size(), 0);
        assert_eq!(term.grid.cursor.point, Point::new(Line(19), Column(0)));
    }

    #[test]
    fn shrink_lines_updates_active_cursor_pos() {
        let mut size = SizeInfo::new(100.0, 10.0, 1.0, 1.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Increase visible lines.
        size.screen_lines.0 = 5;
        term.resize(size);

        assert_eq!(term.history_size(), 15);
        assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
    }

    #[test]
    fn shrink_lines_updates_inactive_cursor_pos() {
        let mut size = SizeInfo::new(100.0, 10.0, 1.0, 1.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);

        // Create 10 lines of scrollback.
        for _ in 0..19 {
            term.newline();
        }
        assert_eq!(term.history_size(), 10);
        assert_eq!(term.grid.cursor.point, Point::new(Line(9), Column(0)));

        // Enter alt screen.
        term.set_mode(ansi::Mode::SwapScreenAndSetRestoreCursor);

        // Increase visible lines.
        size.screen_lines.0 = 5;
        term.resize(size);

        // Leave alt screen.
        term.unset_mode(ansi::Mode::SwapScreenAndSetRestoreCursor);

        assert_eq!(term.history_size(), 15);
        assert_eq!(term.grid.cursor.point, Point::new(Line(4), Column(0)));
    }

    #[test]
    fn window_title() {
        let size = SizeInfo::new(21.0, 51.0, 3.0, 3.0, 0.0, 0.0, false);
        let mut term = Term::new(&MockConfig::default(), size, Mock);

        // Title None by default.
        assert_eq!(term.title, None);

        // Title can be set.
        term.set_title(Some("Test".into()));
        assert_eq!(term.title, Some("Test".into()));

        // Title can be pushed onto stack.
        term.push_title();
        term.set_title(Some("Next".into()));
        assert_eq!(term.title, Some("Next".into()));
        assert_eq!(term.title_stack.get(0).unwrap(), &Some("Test".into()));

        // Title can be popped from stack and set as the window title.
        term.pop_title();
        assert_eq!(term.title, Some("Test".into()));
        assert!(term.title_stack.is_empty());

        // Title stack doesn't grow infinitely.
        for _ in 0..4097 {
            term.push_title();
        }
        assert_eq!(term.title_stack.len(), 4096);

        // Title and title stack reset when terminal state is reset.
        term.push_title();
        term.reset_state();
        assert_eq!(term.title, None);
        assert!(term.title_stack.is_empty());

        // Title stack pops back to default.
        term.title = None;
        term.push_title();
        term.set_title(Some("Test".into()));
        term.pop_title();
        assert_eq!(term.title, None);

        // Title can be reset to default.
        term.title = Some("Test".into());
        term.set_title(None);
        assert_eq!(term.title, None);
    }

    #[test]
    fn parse_cargo_version() {
        assert!(version_number(env!("CARGO_PKG_VERSION")) >= 10_01);
        assert_eq!(version_number("0.0.1-dev"), 1);
        assert_eq!(version_number("0.1.2-dev"), 1_02);
        assert_eq!(version_number("1.2.3-dev"), 1_02_03);
        assert_eq!(version_number("999.99.99"), 9_99_99_99);
    }
}

#[cfg(all(test, feature = "bench"))]
mod benches {
    extern crate serde_json as json;
    extern crate test;

    use std::fs;
    use std::mem;

    use crate::config::MockConfig;
    use crate::event::{Event, EventListener};
    use crate::grid::Grid;

    use super::cell::Cell;
    use super::{SizeInfo, Term};

    struct Mock;
    impl EventListener for Mock {
        fn send_event(&self, _event: Event) {}
    }

    /// Benchmark for the renderable cells iterator.
    ///
    /// The renderable cells iterator yields cells that require work to be
    /// displayed (that is, not an empty background cell). This benchmark
    /// measures how long it takes to process the whole iterator.
    ///
    /// When this benchmark was first added, it averaged ~78usec on my macbook
    /// pro. The total render time for this grid is anywhere between ~1500 and
    /// ~2000usec (measured imprecisely with the visual meter).
    #[bench]
    fn render_iter(b: &mut test::Bencher) {
        // Need some realistic grid state; using one of the ref files.
        let serialized_grid = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ref/vim_large_window_scroll/grid.json"
        ))
        .unwrap();
        let serialized_size = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/ref/vim_large_window_scroll/size.json"
        ))
        .unwrap();

        let mut grid: Grid<Cell> = json::from_str(&serialized_grid).unwrap();
        let size: SizeInfo = json::from_str(&serialized_size).unwrap();

        let config = MockConfig::default();

        let mut terminal = Term::new(&config, size, Mock);
        mem::swap(&mut terminal.grid, &mut grid);

        b.iter(|| {
            let iter = terminal.renderable_cells(&config);
            for cell in iter {
                test::black_box(cell);
            }
        })
    }
}
