use std::cmp::max;
use std::mem;
use std::ops::RangeInclusive;

use alacritty_terminal::ansi::{Color, CursorShape, NamedColor};
use alacritty_terminal::config::Config;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Indexed};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::{CellRgb, Rgb};
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::{
    RenderableContent as TerminalContent, RenderableCursor as TerminalCursor, Term, TermMode,
};

use crate::config::ui_config::UiConfig;
use crate::display::color::{List, DIM_FACTOR};

/// Minimum contrast between a fixed cursor color and the cell's background.
pub const MIN_CURSOR_CONTRAST: f64 = 1.5;

/// Maximum number of linewraps followed outside of the viewport during search highlighting.
const MAX_SEARCH_LINES: usize = 100;

/// Renderable terminal content.
///
/// This provides the terminal cursor and an iterator over all non-empty cells.
pub struct RenderableContent<'a> {
    terminal_content: TerminalContent<'a>,
    terminal_cursor: TerminalCursor,
    cursor: Option<RenderableCursor>,
    search: RenderableSearch,
    config: &'a Config<UiConfig>,
    colors: &'a List,
}

impl<'a> RenderableContent<'a> {
    pub fn new<T: EventListener>(
        term: &'a Term<T>,
        dfas: Option<&RegexSearch>,
        config: &'a Config<UiConfig>,
        colors: &'a List,
        show_cursor: bool,
    ) -> Self {
        let search = dfas.map(|dfas| RenderableSearch::new(&term, dfas)).unwrap_or_default();
        let terminal_content = term.renderable_content();

        // Copy the cursor and override its shape if necessary.
        let mut terminal_cursor = terminal_content.cursor;
        if !show_cursor || terminal_cursor.shape == CursorShape::Hidden {
            terminal_cursor.shape = CursorShape::Hidden;
        } else if !term.is_focused && config.cursor.unfocused_hollow {
            terminal_cursor.shape = CursorShape::HollowBlock;
        }

        Self { cursor: None, terminal_content, terminal_cursor, search, config, colors }
    }

    /// Viewport offset.
    pub fn display_offset(&self) -> usize {
        self.terminal_content.display_offset
    }

    /// Get the terminal cursor.
    pub fn cursor(mut self) -> Option<RenderableCursor> {
        // Drain the iterator to make sure the cursor is created.
        while self.next().is_some() && self.cursor.is_none() {}

        self.cursor
    }

    /// Get the RGB value for a color index.
    pub fn color(&self, color: usize) -> Rgb {
        self.terminal_content.colors[color].unwrap_or(self.colors[color])
    }

    /// Assemble the information required to render the terminal cursor.
    ///
    /// This will return `None` when there is no cursor visible.
    fn renderable_cursor(&mut self, cell: &RenderableCell) -> Option<RenderableCursor> {
        if self.terminal_cursor.shape == CursorShape::Hidden {
            return None;
        }

        // Expand across wide cell when inside wide char or spacer.
        let is_wide = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            self.terminal_cursor.point.column -= 1;
            true
        } else {
            cell.flags.contains(Flags::WIDE_CHAR)
        };

        // Cursor colors.
        let color = if self.terminal_content.mode.contains(TermMode::VI) {
            self.config.ui_config.colors.vi_mode_cursor
        } else {
            self.config.ui_config.colors.cursor
        };
        let mut cursor_color =
            self.terminal_content.colors[NamedColor::Cursor].map_or(color.background, CellRgb::Rgb);
        let mut text_color = color.foreground;

        // Invert the cursor if it has a fixed background close to the cell's background.
        if matches!(
            cursor_color,
            CellRgb::Rgb(color) if color.contrast(cell.bg) < MIN_CURSOR_CONTRAST
        ) {
            cursor_color = CellRgb::CellForeground;
            text_color = CellRgb::CellBackground;
        }

        // Convert from cell colors to RGB.
        let text_color = text_color.color(cell.fg, cell.bg);
        let cursor_color = cursor_color.color(cell.fg, cell.bg);

        Some(RenderableCursor {
            point: self.terminal_cursor.point,
            shape: self.terminal_cursor.shape,
            cursor_color,
            text_color,
            is_wide,
        })
    }
}

impl<'a> Iterator for RenderableContent<'a> {
    type Item = RenderableCell;

    /// Gets the next renderable cell.
    ///
    /// Skips empty (background) cells and applies any flags to the cell state
    /// (eg. invert fg and bg colors).
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let cell = self.terminal_content.display_iter.next()?;
            let mut cell = RenderableCell::new(self, cell);

            if self.terminal_cursor.point == cell.point {
                // Store the cursor which should be rendered.
                self.cursor = self.renderable_cursor(&cell).map(|cursor| {
                    if cursor.shape == CursorShape::Block {
                        cell.fg = cursor.text_color;
                        cell.bg = cursor.cursor_color;

                        // Since we draw Block cursor by drawing cell below it with a proper color,
                        // we must adjust alpha to make it visible.
                        cell.bg_alpha = 1.;
                    }

                    cursor
                });

                return Some(cell);
            } else if !cell.is_empty() && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                // Skip empty cells and wide char spacers.
                return Some(cell);
            }
        }
    }
}

/// Cell ready for rendering.
#[derive(Clone, Debug)]
pub struct RenderableCell {
    pub character: char,
    pub zerowidth: Option<Vec<char>>,
    pub point: Point,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bg_alpha: f32,
    pub underline_color: Rgb,
    pub flags: Flags,
    pub is_match: bool,
}

impl RenderableCell {
    fn new<'a>(content: &mut RenderableContent<'a>, cell: Indexed<&Cell, Line>) -> Self {
        // Lookup RGB values.
        let mut fg_rgb = Self::compute_fg_rgb(content, cell.fg, cell.flags);
        let mut bg_rgb = Self::compute_bg_rgb(content, cell.bg);

        let mut bg_alpha = if cell.flags.contains(Flags::INVERSE) {
            mem::swap(&mut fg_rgb, &mut bg_rgb);
            1.0
        } else {
            Self::compute_bg_alpha(cell.bg)
        };

        let is_selected = content
            .terminal_content
            .selection
            .map_or(false, |selection| selection.contains_cell(&cell, content.terminal_cursor));
        let mut is_match = false;

        let colors = &content.config.ui_config.colors;
        if is_selected {
            let config_bg = colors.selection.background;
            let selected_fg = colors.selection.foreground.color(fg_rgb, bg_rgb);
            bg_rgb = config_bg.color(fg_rgb, bg_rgb);
            fg_rgb = selected_fg;

            if fg_rgb == bg_rgb && !cell.flags.contains(Flags::HIDDEN) {
                // Reveal inversed text when fg/bg is the same.
                fg_rgb = content.color(NamedColor::Background as usize);
                bg_rgb = content.color(NamedColor::Foreground as usize);
                bg_alpha = 1.0;
            } else if config_bg != CellRgb::CellBackground {
                bg_alpha = 1.0;
            }
        } else if content.search.advance(cell.point) {
            // Highlight the cell if it is part of a search match.
            let config_bg = colors.search.matches.background;
            let matched_fg = colors.search.matches.foreground.color(fg_rgb, bg_rgb);
            bg_rgb = config_bg.color(fg_rgb, bg_rgb);
            fg_rgb = matched_fg;

            if config_bg != CellRgb::CellBackground {
                bg_alpha = 1.0;
            }

            is_match = true;
        }

        let (zerowidth, underline_color) = if let Some(extra) = cell.extra() {
            (Some(extra.zerowidth.clone()), extra.underline_color)
        } else {
            (None, None)
        };

        let underline_rgb = underline_color
            .map_or(fg_rgb, |color| Self::compute_fg_rgb(content, color, cell.flags));

        RenderableCell {
            character: cell.c,
            zerowidth,
            point: cell.point,
            fg: fg_rgb,
            bg: bg_rgb,
            bg_alpha,
            underline_color: underline_rgb,
            flags: cell.flags,
            is_match,
        }
    }

    /// Check if cell contains any renderable content.
    fn is_empty(&self) -> bool {
        self.bg_alpha == 0.
            && !self.flags.intersects(Flags::UNDERLINE | Flags::STRIKEOUT | Flags::DOUBLE_UNDERLINE)
            && self.character == ' '
            && self.zerowidth.is_none()
    }

    /// Get the RGB color from a cell's foreground color.
    fn compute_fg_rgb(content: &mut RenderableContent<'_>, fg: Color, flags: Flags) -> Rgb {
        let ui_config = &content.config.ui_config;
        match fg {
            Color::Spec(rgb) => match flags & Flags::DIM {
                Flags::DIM => rgb * DIM_FACTOR,
                _ => rgb,
            },
            Color::Named(ansi) => {
                match (ui_config.draw_bold_text_with_bright_colors, flags & Flags::DIM_BOLD) {
                    // If no bright foreground is set, treat it like the BOLD flag doesn't exist.
                    (_, Flags::DIM_BOLD)
                        if ansi == NamedColor::Foreground
                            && ui_config.colors.primary.bright_foreground.is_none() =>
                    {
                        content.color(NamedColor::DimForeground as usize)
                    },
                    // Draw bold text in bright colors *and* contains bold flag.
                    (true, Flags::BOLD) => content.color(ansi.to_bright() as usize),
                    // Cell is marked as dim and not bold.
                    (_, Flags::DIM) | (false, Flags::DIM_BOLD) => {
                        content.color(ansi.to_dim() as usize)
                    },
                    // None of the above, keep original color..
                    _ => content.color(ansi as usize),
                }
            },
            Color::Indexed(idx) => {
                let idx = match (
                    ui_config.draw_bold_text_with_bright_colors,
                    flags & Flags::DIM_BOLD,
                    idx,
                ) {
                    (true, Flags::BOLD, 0..=7) => idx as usize + 8,
                    (false, Flags::DIM, 8..=15) => idx as usize - 8,
                    (false, Flags::DIM, 0..=7) => NamedColor::DimBlack as usize + idx as usize,
                    _ => idx as usize,
                };

                content.color(idx)
            },
        }
    }

    /// Get the RGB color from a cell's background color.
    #[inline]
    fn compute_bg_rgb(content: &mut RenderableContent<'_>, bg: Color) -> Rgb {
        match bg {
            Color::Spec(rgb) => rgb,
            Color::Named(ansi) => content.color(ansi as usize),
            Color::Indexed(idx) => content.color(idx as usize),
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
}

/// Cursor storing all information relevant for rendering.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct RenderableCursor {
    shape: CursorShape,
    cursor_color: Rgb,
    text_color: Rgb,
    is_wide: bool,
    point: Point,
}

impl RenderableCursor {
    pub fn color(&self) -> Rgb {
        self.cursor_color
    }

    pub fn shape(&self) -> CursorShape {
        self.shape
    }

    pub fn is_wide(&self) -> bool {
        self.is_wide
    }

    pub fn point(&self) -> Point {
        self.point
    }
}

/// Regex search highlight tracking.
#[derive(Default)]
pub struct RenderableSearch {
    /// All visible search matches.
    matches: Vec<RangeInclusive<Point>>,

    /// Index of the last match checked.
    index: usize,
}

impl RenderableSearch {
    /// Create a new renderable search iterator.
    pub fn new<T>(term: &Term<T>, dfas: &RegexSearch) -> Self {
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
                return Self::default();
            } else {
                // Start at next line if this one is too long.
                start.line -= 1;
            }
        }
        end.line = max(end.line, viewport_end.saturating_sub(MAX_SEARCH_LINES));

        // Create an iterater for the current regex search for all visible matches.
        let iter = RegexIter::new(start, end, Direction::Right, term, dfas)
            .skip_while(move |rm| rm.end().line > viewport_start)
            .take_while(move |rm| rm.start().line >= viewport_end)
            .map(|rm| {
                let viewport_start = term.grid().clamp_buffer_to_visible(*rm.start());
                let viewport_end = term.grid().clamp_buffer_to_visible(*rm.end());
                viewport_start..=viewport_end
            });

        Self { matches: iter.collect(), index: 0 }
    }

    /// Advance the search tracker to the next point.
    ///
    /// This will return `true` if the point passed is part of a search match.
    fn advance(&mut self, point: Point) -> bool {
        while let Some(regex_match) = self.matches.get(self.index) {
            if regex_match.start() > &point {
                break;
            } else if regex_match.end() < &point {
                self.index += 1;
            } else {
                return true;
            }
        }
        false
    }
}
