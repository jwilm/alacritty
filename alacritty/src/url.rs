use std::cmp::min;
use std::mem;

use crossfont::Metrics;
use glutin::event::{ElementState, ModifiersState};
use urlocator::{UrlLocation, UrlLocator};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Rgb;
use alacritty_terminal::term::SizeInfo;

use crate::config::Config;
use crate::display::content::RenderableCell;
use crate::event::Mouse;
use crate::renderer::rects::{RenderLine, RenderRect};

#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    lines: Vec<RenderLine>,
    end_offset: u16,
    size: SizeInfo,
}

impl Url {
    /// Rectangles required for underlining the URL.
    pub fn rects(&self, metrics: &Metrics, size: &SizeInfo) -> Vec<RenderRect> {
        let end = self.end();
        self.lines
            .iter()
            .filter(|line| line.start <= end)
            .map(|line| {
                let mut rect_line = *line;
                rect_line.end = min(line.end, end);
                rect_line.rects(Flags::UNDERLINE, metrics, size)
            })
            .flatten()
            .collect()
    }

    /// Viewport start point of the URL.
    pub fn start(&self) -> Point<usize> {
        self.lines[0].start
    }

    /// Viewport end point of the URL.
    pub fn end(&self) -> Point<usize> {
        let end = self.lines[self.lines.len() - 1].end;

        // Convert to Point<Line> to make use of the grid clamping logic.
        let mut end = Point::new(Line(end.line as i32), end.column);
        end = end.sub(&self.size, Boundary::Cursor, self.end_offset as usize);

        Point::new(end.line.0 as usize, end.column)
    }
}

pub struct Urls {
    locator: UrlLocator,
    urls: Vec<Url>,
    scheme_buffer: Vec<(Point<usize>, Rgb)>,
    next_point: Point<usize>,
    state: UrlLocation,
}

impl Default for Urls {
    fn default() -> Self {
        Self {
            locator: UrlLocator::new(),
            scheme_buffer: Vec::new(),
            urls: Vec::new(),
            state: UrlLocation::Reset,
            next_point: Point::new(0, Column(0)),
        }
    }
}

impl Urls {
    pub fn new() -> Self {
        Self::default()
    }

    // Update tracked URLs.
    pub fn update(&mut self, size: &SizeInfo, cell: &RenderableCell) {
        let point = cell.point;
        let mut end = point;

        // Include the following wide char spacer.
        if cell.flags.contains(Flags::WIDE_CHAR) {
            end.column += 1;
        }

        // Reset URL when empty cells have been skipped.
        if point != Point::new(0, Column(0)) && point != self.next_point {
            self.reset();
        }

        self.next_point = if end.column.0 + 1 == size.columns() {
            Point::new(end.line + 1, Column(0))
        } else {
            Point::new(end.line, end.column + 1)
        };

        // Extend current state if a leading wide char spacer is encountered.
        if cell.flags.intersects(Flags::LEADING_WIDE_CHAR_SPACER) {
            if let UrlLocation::Url(_, mut end_offset) = self.state {
                if end_offset != 0 {
                    end_offset += 1;
                }

                self.extend_url(point, end, cell.fg, end_offset);
            }

            return;
        }

        // Advance parser.
        let last_state = mem::replace(&mut self.state, self.locator.advance(cell.character));
        match (self.state, last_state) {
            (UrlLocation::Url(_length, end_offset), UrlLocation::Scheme) => {
                // Create empty URL.
                self.urls.push(Url { lines: Vec::new(), end_offset, size: *size });

                // Push schemes into URL.
                for (scheme_point, scheme_fg) in self.scheme_buffer.split_off(0) {
                    self.extend_url(scheme_point, scheme_point, scheme_fg, end_offset);
                }

                // Push the new cell into URL.
                self.extend_url(point, end, cell.fg, end_offset);
            },
            (UrlLocation::Url(_length, end_offset), UrlLocation::Url(..)) => {
                self.extend_url(point, end, cell.fg, end_offset);
            },
            (UrlLocation::Scheme, _) => self.scheme_buffer.push((cell.point, cell.fg)),
            (UrlLocation::Reset, _) => self.reset(),
            _ => (),
        }

        // Reset at un-wrapped linebreak.
        if cell.point.column.0 + 1 == size.columns() && !cell.flags.contains(Flags::WRAPLINE) {
            self.reset();
        }
    }

    /// Extend the last URL.
    fn extend_url(&mut self, start: Point<usize>, end: Point<usize>, color: Rgb, end_offset: u16) {
        let url = self.urls.last_mut().unwrap();

        // If color changed, we need to insert a new line.
        if url.lines.last().map(|last| last.color) == Some(color) {
            url.lines.last_mut().unwrap().end = end;
        } else {
            url.lines.push(RenderLine { start, end, color });
        }

        // Update excluded cells at the end of the URL.
        url.end_offset = end_offset;
    }

    /// Find URL below the mouse cursor.
    pub fn highlighted(
        &self,
        config: &Config,
        mouse: &Mouse,
        mods: ModifiersState,
        mouse_mode: bool,
        selection: bool,
    ) -> Option<Url> {
        // Require additional shift in mouse mode.
        let mut required_mods = config.ui_config.mouse.url.mods();
        if mouse_mode {
            required_mods |= ModifiersState::SHIFT;
        }

        // Make sure all prerequisites for highlighting are met.
        if selection
            || !mouse.inside_text_area
            || config.ui_config.mouse.url.launcher.is_none()
            || required_mods != mods
            || mouse.left_button_state == ElementState::Pressed
        {
            return None;
        }

        self.find_at(mouse.point)
    }

    /// Find URL at location.
    pub fn find_at(&self, point: Point<usize>) -> Option<Url> {
        for url in &self.urls {
            if (url.start()..=url.end()).contains(&point) {
                return Some(url.clone());
            }
        }
        None
    }

    fn reset(&mut self) {
        self.locator = UrlLocator::new();
        self.state = UrlLocation::Reset;
        self.scheme_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alacritty_terminal::index::Column;

    fn text_to_cells(text: &str) -> Vec<RenderableCell> {
        text.chars()
            .enumerate()
            .map(|(i, character)| RenderableCell {
                character,
                zerowidth: None,
                graphic: None,
                point: Point::new(0, Column(i)),
                fg: Default::default(),
                bg: Default::default(),
                bg_alpha: 0.,
                flags: Flags::empty(),
            })
            .collect()
    }

    #[test]
    fn multi_color_url() {
        let mut input = text_to_cells("test https://example.org ing");
        let size = SizeInfo::new(input.len() as f32, 1., 1.0, 1.0, 0.0, 0.0, false);

        input[10].fg = Rgb { r: 0xff, g: 0x00, b: 0xff };

        let mut urls = Urls::new();

        for cell in input {
            urls.update(&size, &cell);
        }

        let url = urls.urls.first().unwrap();
        assert_eq!(url.start().column, Column(5));
        assert_eq!(url.end().column, Column(23));
    }

    #[test]
    fn multiple_urls() {
        let input = text_to_cells("test git:a git:b git:c ing");
        let size = SizeInfo::new(input.len() as f32, 1., 1.0, 1.0, 0.0, 0.0, false);

        let mut urls = Urls::new();

        for cell in input {
            urls.update(&size, &cell);
        }

        assert_eq!(urls.urls.len(), 3);

        assert_eq!(urls.urls[0].start().column, Column(5));
        assert_eq!(urls.urls[0].end().column, Column(9));

        assert_eq!(urls.urls[1].start().column, Column(11));
        assert_eq!(urls.urls[1].end().column, Column(15));

        assert_eq!(urls.urls[2].start().column, Column(17));
        assert_eq!(urls.urls[2].end().column, Column(21));
    }

    #[test]
    fn wide_urls() {
        let input = text_to_cells("test https://こんにちは (http:여보세요) ing");
        let size = SizeInfo::new(input.len() as f32 + 9., 1., 1.0, 1.0, 0.0, 0.0, false);

        let mut urls = Urls::new();

        for cell in input {
            urls.update(&size, &cell);
        }

        assert_eq!(urls.urls.len(), 2);

        assert_eq!(urls.urls[0].start().column, Column(5));
        assert_eq!(urls.urls[0].end().column, Column(17));

        assert_eq!(urls.urls[1].start().column, Column(20));
        assert_eq!(urls.urls[1].end().column, Column(28));
    }
}
