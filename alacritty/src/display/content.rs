use std::borrow::Cow;
use std::cmp::{max, min};
use std::mem;
use std::ops::{Deref, DerefMut, RangeInclusive};

use alacritty_terminal::ansi::{Color, CursorShape, NamedColor};
use alacritty_terminal::config::Config;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Indexed};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::{CellRgb, Rgb};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{RenderableContent as TerminalContent, Term, TermMode};

use crate::config::ui_config::UiConfig;
use crate::display::color::{List, DIM_FACTOR};
use crate::display::hint::HintState;
use crate::display::{self, Display, MAX_SEARCH_LINES};
use crate::event::SearchState;

/// Minimum contrast between a fixed cursor color and the cell's background.
pub const MIN_CURSOR_CONTRAST: f64 = 1.5;

/// Renderable terminal content.
///
/// This provides the terminal cursor and an iterator over all non-empty cells.
pub struct RenderableContent<'a> {
    terminal_content: TerminalContent<'a>,
    cursor: Option<RenderableCursor>,
    cursor_shape: CursorShape,
    cursor_point: Point<usize>,
    search: Option<Regex<'a>>,
    hint: Option<Hint<'a>>,
    config: &'a Config<UiConfig>,
    colors: &'a List,
    focused_match: Option<&'a Match>,
}

impl<'a> RenderableContent<'a> {
    pub fn new<T: EventListener>(
        config: &'a Config<UiConfig>,
        display: &'a mut Display,
        term: &'a Term<T>,
        search_state: &'a SearchState,
    ) -> Self {
        let search = search_state.dfas().map(|dfas| Regex::new(term, dfas));
        let focused_match = search_state.focused_match();
        let terminal_content = term.renderable_content();

        // Find terminal cursor shape.
        let cursor_shape = if terminal_content.cursor.shape == CursorShape::Hidden
            || display.cursor_hidden
            || search_state.regex().is_some()
        {
            CursorShape::Hidden
        } else if !term.is_focused && config.cursor.unfocused_hollow {
            CursorShape::HollowBlock
        } else {
            terminal_content.cursor.shape
        };

        // Convert terminal cursor point to viewport position.
        let cursor_point = terminal_content.cursor.point;
        let display_offset = terminal_content.display_offset;
        let cursor_point = display::point_to_viewport(display_offset, cursor_point).unwrap();

        let hint = if display.hint_state.active() {
            display.hint_state.update_matches(term);
            Some(Hint::from(&display.hint_state))
        } else {
            None
        };

        Self {
            colors: &display.colors,
            cursor: None,
            terminal_content,
            focused_match,
            cursor_shape,
            cursor_point,
            search,
            config,
            hint,
        }
    }

    /// Viewport offset.
    pub fn display_offset(&self) -> usize {
        self.terminal_content.display_offset
    }

    /// Get the terminal cursor.
    pub fn cursor(mut self) -> Option<RenderableCursor> {
        // Assure this function is only called after the iterator has been drained.
        debug_assert!(self.next().is_none());

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
        if self.cursor_shape == CursorShape::Hidden {
            return None;
        }

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
            is_wide: cell.flags.contains(Flags::WIDE_CHAR),
            shape: self.cursor_shape,
            point: self.cursor_point,
            cursor_color,
            text_color,
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

            if self.cursor_point == cell.point {
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
    pub point: Point<usize>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bg_alpha: f32,
    pub flags: Flags,
}

impl RenderableCell {
    fn new<'a>(content: &mut RenderableContent<'a>, cell: Indexed<&Cell>) -> Self {
        // Lookup RGB values.
        let mut fg = Self::compute_fg_rgb(content, cell.fg, cell.flags);
        let mut bg = Self::compute_bg_rgb(content, cell.bg);

        let mut bg_alpha = if cell.flags.contains(Flags::INVERSE) {
            mem::swap(&mut fg, &mut bg);
            1.0
        } else {
            Self::compute_bg_alpha(content, cell.bg)
        };

        let is_selected = content.terminal_content.selection.map_or(false, |selection| {
            selection.contains_cell(
                &cell,
                content.terminal_content.cursor.point,
                content.cursor_shape,
            )
        });

        let display_offset = content.terminal_content.display_offset;
        let viewport_start = Point::new(Line(-(display_offset as i32)), Column(0));
        let colors = &content.config.ui_config.colors;
        let mut character = cell.c;

        if let Some((c, is_first)) =
            content.hint.as_mut().and_then(|hint| hint.advance(viewport_start, cell.point))
        {
            let (config_fg, config_bg) = if is_first {
                (colors.hints.start.foreground, colors.hints.start.background)
            } else {
                (colors.hints.end.foreground, colors.hints.end.background)
            };
            Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);

            character = c;
        } else if is_selected {
            let config_fg = colors.selection.foreground;
            let config_bg = colors.selection.background;
            Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);

            if fg == bg && !cell.flags.contains(Flags::HIDDEN) {
                // Reveal inversed text when fg/bg is the same.
                fg = content.color(NamedColor::Background as usize);
                bg = content.color(NamedColor::Foreground as usize);
                bg_alpha = 1.0;
            }
        } else if content.search.as_mut().map_or(false, |search| search.advance(cell.point)) {
            let focused = content.focused_match.map_or(false, |fm| fm.contains(&cell.point));
            let (config_fg, config_bg) = if focused {
                (colors.search.focused_match.foreground, colors.search.focused_match.background)
            } else {
                (colors.search.matches.foreground, colors.search.matches.background)
            };
            Self::compute_cell_rgb(&mut fg, &mut bg, &mut bg_alpha, config_fg, config_bg);
        }

        // Convert cell point to viewport position.
        let cell_point = cell.point;
        let point = display::point_to_viewport(display_offset, cell_point).unwrap();

        RenderableCell {
            zerowidth: cell.zerowidth().map(|zerowidth| zerowidth.to_vec()),
            flags: cell.flags,
            character,
            bg_alpha,
            point,
            fg,
            bg,
        }
    }

    /// Check if cell contains any renderable content.
    fn is_empty(&self) -> bool {
        self.bg_alpha == 0.
            && self.character == ' '
            && self.zerowidth.is_none()
            && !self.flags.intersects(Flags::UNDERLINE | Flags::STRIKEOUT | Flags::DOUBLE_UNDERLINE)
    }

    /// Apply [`CellRgb`] colors to the cell's colors.
    fn compute_cell_rgb(
        cell_fg: &mut Rgb,
        cell_bg: &mut Rgb,
        bg_alpha: &mut f32,
        fg: CellRgb,
        bg: CellRgb,
    ) {
        let old_fg = mem::replace(cell_fg, fg.color(*cell_fg, *cell_bg));
        *cell_bg = bg.color(old_fg, *cell_bg);

        if bg != CellRgb::CellBackground {
            *bg_alpha = 1.0;
        }
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
    fn compute_bg_alpha(content: &mut RenderableContent<'_>, bg: Color) -> f32 {
        if bg == Color::Named(NamedColor::Background) {
            0.
        } else if content.config.ui_config.colors.opaque_background_colors {
            1.
        } else {
            content.config.ui_config.background_opacity()
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
    point: Point<usize>,
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

    pub fn point(&self) -> Point<usize> {
        self.point
    }
}

/// Regex hints for keyboard shortcuts.
struct Hint<'a> {
    /// Hint matches and position.
    regex: Regex<'a>,

    /// Last match checked against current cell position.
    labels: &'a Vec<Vec<char>>,
}

impl<'a> Hint<'a> {
    /// Advance the hint iterator.
    ///
    /// If the point is within a hint, the keyboard shortcut character that should be displayed at
    /// this position will be returned.
    ///
    /// The tuple's [`bool`] will be `true` when the character is the first for this hint.
    fn advance(&mut self, viewport_start: Point, point: Point) -> Option<(char, bool)> {
        // Check if we're within a match at all.
        if !self.regex.advance(point) {
            return None;
        }

        // Match starting position on this line; linebreaks interrupt the hint labels.
        let start = self
            .regex
            .matches
            .get(self.regex.index)
            .map(|regex_match| max(*regex_match.start(), viewport_start))
            .filter(|start| start.line == point.line)?;

        // Position within the hint label.
        let label_position = point.column.0 - start.column.0;
        let is_first = label_position == 0;

        // Hint label character.
        self.labels[self.regex.index].get(label_position).copied().map(|c| (c, is_first))
    }
}

impl<'a> From<&'a HintState> for Hint<'a> {
    fn from(hint_state: &'a HintState) -> Self {
        let regex = Regex { matches: Cow::Borrowed(hint_state.matches()), index: 0 };
        Self { labels: hint_state.labels(), regex }
    }
}

/// Wrapper for finding visible regex matches.
#[derive(Default, Clone)]
pub struct RegexMatches(pub Vec<RangeInclusive<Point>>);

impl RegexMatches {
    /// Find all visible matches.
    pub fn new<T>(term: &Term<T>, dfas: &RegexSearch) -> Self {
        let viewport_start = Line(-(term.grid().display_offset() as i32));
        let viewport_end = viewport_start + term.bottommost_line();

        // Compute start of the first and end of the last line.
        let start_point = Point::new(viewport_start, Column(0));
        let mut start = term.line_search_left(start_point);
        let end_point = Point::new(viewport_end, term.last_column());
        let mut end = term.line_search_right(end_point);

        // Set upper bound on search before/after the viewport to prevent excessive blocking.
        start.line = max(start.line, viewport_start - MAX_SEARCH_LINES);
        end.line = min(end.line, viewport_end + MAX_SEARCH_LINES);

        // Create an iterater for the current regex search for all visible matches.
        let iter = RegexIter::new(start, end, Direction::Right, term, dfas)
            .skip_while(move |rm| rm.end().line < viewport_start)
            .take_while(move |rm| rm.start().line <= viewport_end);

        Self(iter.collect())
    }
}

impl Deref for RegexMatches {
    type Target = Vec<RangeInclusive<Point>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RegexMatches {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Visible regex match tracking.
#[derive(Default)]
struct Regex<'a> {
    /// All visible matches.
    matches: Cow<'a, RegexMatches>,

    /// Index of the last match checked.
    index: usize,
}

impl<'a> Regex<'a> {
    /// Create a new renderable regex iterator.
    fn new<T>(term: &Term<T>, dfas: &RegexSearch) -> Self {
        let matches = Cow::Owned(RegexMatches::new(term, dfas));
        Self { index: 0, matches }
    }

    /// Advance the regex tracker to the next point.
    ///
    /// This will return `true` if the point passed is part of a regex match.
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
