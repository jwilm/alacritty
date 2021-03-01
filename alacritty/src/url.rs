use std::cmp::min;
use std::mem;

use crossfont::Metrics;
use glutin::event::{ElementState, ModifiersState};
use urlocator::{UrlLocation, UrlLocator};

use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Rgb;
use alacritty_terminal::term::SizeInfo;

use crate::config::Config;
use crate::display::content::RenderableCell;
use crate::event::Mouse;
use crate::renderer::rects::{RenderLine, RenderRect};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    lines: Vec<RenderLine>,
    end_offset: u16,
    num_cols: Column,
}

impl Url {
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

    pub fn start(&self) -> Point {
        self.lines[0].start
    }

    pub fn end(&self) -> Point {
        self.lines[self.lines.len() - 1].end.sub(self.num_cols, self.end_offset as usize)
    }
}

pub struct Urls {
    locator: UrlLocator,
    urls: Vec<Url>,
    scheme_buffer: Vec<(Point, Rgb)>,
    last_point: Option<Point>,
    state: UrlLocation,
}

impl Default for Urls {
    fn default() -> Self {
        Self {
            locator: UrlLocator::new(),
            scheme_buffer: Vec::new(),
            urls: Vec::new(),
            state: UrlLocation::Reset,
            last_point: None,
        }
    }
}

impl Urls {
    pub fn new() -> Self {
        Self::default()
    }

    // Update tracked URLs.
    pub fn update(&mut self, num_cols: Column, cell: &RenderableCell) {
        let point = cell.point;
        let mut end = point;

        // Include the following wide char spacer.
        if cell.flags.contains(Flags::WIDE_CHAR) {
            end.column += 1;
        }

        // Reset URL when empty cells have been skipped.
        if point != Point::default() && Some(point.sub(num_cols, 1)) != self.last_point {
            self.reset();
        }

        self.last_point = Some(end);

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
                self.urls.push(Url { lines: Vec::new(), end_offset, num_cols });

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
        if cell.point.column + 1 == num_cols && !cell.flags.contains(Flags::WRAPLINE) {
            self.reset();
        }
    }

    /// Extend the last URL.
    fn extend_url(&mut self, start: Point, end: Point, color: Rgb, end_offset: u16) {
        let url = self.urls.last_mut().unwrap();

        // If color changed, we need to insert a new line.
        if url.lines.last().map(|last| last.color) == Some(color) {
            url.lines.last_mut().unwrap().end = end;
        } else {
            url.lines.push(RenderLine { color, start, end });
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

        self.find_at(Point::new(mouse.line, mouse.column))
    }

    /// Find URL at location.
    pub fn find_at(&self, point: Point) -> Option<Url> {
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

    use alacritty_terminal::index::{Column, Line};

    fn text_to_cells(text: &str) -> Vec<RenderableCell> {
        text.chars()
            .enumerate()
            .map(|(i, character)| RenderableCell {
                character,
                zerowidth: None,
                point: Point::new(Line(0), Column(i)),
                fg: Default::default(),
                bg: Default::default(),
                underline_color: Default::default(),
                bg_alpha: 0.,
                flags: Flags::empty(),
                is_match: false,
            })
            .collect()
    }

    #[test]
    fn multi_color_url() {
        let mut input = text_to_cells("test https://example.org ing");
        let num_cols = input.len();

        input[10].fg = Rgb { r: 0xff, g: 0x00, b: 0xff };

        let mut urls = Urls::new();

        for cell in input {
            urls.update(Column(num_cols), &cell);
        }

        let url = urls.urls.first().unwrap();
        assert_eq!(url.start().column, Column(5));
        assert_eq!(url.end().column, Column(23));
    }

    #[test]
    fn multiple_urls() {
        let input = text_to_cells("test git:a git:b git:c ing");
        let num_cols = input.len();

        let mut urls = Urls::new();

        for cell in input {
            urls.update(Column(num_cols), &cell);
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
        let num_cols = input.len() + 9;

        let mut urls = Urls::new();

        for cell in input {
            urls.update(Column(num_cols), &cell);
        }

        assert_eq!(urls.urls.len(), 2);

        assert_eq!(urls.urls[0].start().column, Column(5));
        assert_eq!(urls.urls[0].end().column, Column(17));

        assert_eq!(urls.urls[1].start().column, Column(20));
        assert_eq!(urls.urls[1].end().column, Column(28));
    }
}
