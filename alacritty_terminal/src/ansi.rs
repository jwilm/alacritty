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
//! ANSI Terminal Stream Parsing
use std::io;
use std::str;

use base64;
use log::{debug, trace};
use serde::{Deserialize, Serialize};

use vte;

use crate::index::{Column, Contains, Line};
use crate::term::color::Rgb;

// Parse colors in XParseColor format
fn xparse_color(color: &[u8]) -> Option<Rgb> {
    if !color.is_empty() && color[0] == b'#' {
        parse_legacy_color(&color[1..])
    } else if color.len() >= 4 && &color[..4] == b"rgb:" {
        parse_rgb_color(&color[4..])
    } else {
        None
    }
}

// Parse colors in `rgb:r(rrr)/g(ggg)/b(bbb)` format
fn parse_rgb_color(color: &[u8]) -> Option<Rgb> {
    let colors = str::from_utf8(color).ok()?.split('/').collect::<Vec<_>>();

    if colors.len() != 3 {
        return None;
    }

    // Scale values instead of filling with `0`s
    let scale = |input: &str| {
        let max = u32::pow(16, input.len() as u32) - 1;
        let value = u32::from_str_radix(input, 16).ok()?;
        Some((255 * value / max) as u8)
    };

    Some(Rgb { r: scale(colors[0])?, g: scale(colors[1])?, b: scale(colors[2])? })
}

// Parse colors in `#r(rrr)g(ggg)b(bbb)` format
fn parse_legacy_color(color: &[u8]) -> Option<Rgb> {
    let item_len = color.len() / 3;

    // Truncate/Fill to two byte precision
    let color_from_slice = |slice: &[u8]| {
        let col = usize::from_str_radix(str::from_utf8(slice).ok()?, 16).ok()? << 4;
        Some((col >> (4 * slice.len().saturating_sub(1))) as u8)
    };

    Some(Rgb {
        r: color_from_slice(&color[0..item_len])?,
        g: color_from_slice(&color[item_len..item_len * 2])?,
        b: color_from_slice(&color[item_len * 2..])?,
    })
}

fn parse_number(input: &[u8]) -> Option<u8> {
    if input.is_empty() {
        return None;
    }
    let mut num: u8 = 0;
    for c in input {
        let c = *c as char;
        if let Some(digit) = c.to_digit(10) {
            num = match num.checked_mul(10).and_then(|v| v.checked_add(digit as u8)) {
                Some(v) => v,
                None => return None,
            }
        } else {
            return None;
        }
    }
    Some(num)
}

/// The processor wraps a `vte::Parser` to ultimately call methods on a Handler
pub struct Processor {
    state: ProcessorState,
    parser: vte::Parser,
}

/// Internal state for VTE processor
struct ProcessorState {
    preceding_char: Option<char>,
}

/// Helper type that implements `vte::Perform`.
///
/// Processor creates a Performer when running advance and passes the Performer
/// to `vte::Parser`.
struct Performer<'a, H: Handler + TermInfo, W: io::Write> {
    state: &'a mut ProcessorState,
    handler: &'a mut H,
    writer: &'a mut W,
}

impl<'a, H: Handler + TermInfo + 'a, W: io::Write> Performer<'a, H, W> {
    /// Create a performer
    #[inline]
    pub fn new<'b>(
        state: &'b mut ProcessorState,
        handler: &'b mut H,
        writer: &'b mut W,
    ) -> Performer<'b, H, W> {
        Performer { state, handler, writer }
    }
}

impl Default for Processor {
    fn default() -> Processor {
        Processor { state: ProcessorState { preceding_char: None }, parser: vte::Parser::new() }
    }
}

impl Processor {
    pub fn new() -> Processor {
        Default::default()
    }

    #[inline]
    pub fn advance<H, W>(&mut self, handler: &mut H, byte: u8, writer: &mut W)
    where
        H: Handler + TermInfo,
        W: io::Write,
    {
        let mut performer = Performer::new(&mut self.state, handler, writer);
        self.parser.advance(&mut performer, byte);
    }
}

/// Trait that provides properties of terminal
pub trait TermInfo {
    fn lines(&self) -> Line;
    fn cols(&self) -> Column;
}

/// Type that handles actions from the parser
///
/// XXX Should probably not provide default impls for everything, but it makes
/// writing specific handler impls for tests far easier.
pub trait Handler {
    /// OSC to set window title
    fn set_title(&mut self, _: &str) {}

    /// Set the cursor style
    fn set_cursor_style(&mut self, _: Option<CursorStyle>) {}

    /// A character to be displayed
    fn input(&mut self, _c: char) {}

    /// Set cursor to position
    fn goto(&mut self, _: Line, _: Column) {}

    /// Set cursor to specific row
    fn goto_line(&mut self, _: Line) {}

    /// Set cursor to specific column
    fn goto_col(&mut self, _: Column) {}

    /// Insert blank characters in current line starting from cursor
    fn insert_blank(&mut self, _: Column) {}

    /// Move cursor up `rows`
    fn move_up(&mut self, _: Line) {}

    /// Move cursor down `rows`
    fn move_down(&mut self, _: Line) {}

    /// Identify the terminal (should write back to the pty stream)
    ///
    /// TODO this should probably return an io::Result
    fn identify_terminal<W: io::Write>(&mut self, _: &mut W) {}

    // Report device status
    fn device_status<W: io::Write>(&mut self, _: &mut W, _: usize) {}

    /// Move cursor forward `cols`
    fn move_forward(&mut self, _: Column) {}

    /// Move cursor backward `cols`
    fn move_backward(&mut self, _: Column) {}

    /// Move cursor down `rows` and set to column 1
    fn move_down_and_cr(&mut self, _: Line) {}

    /// Move cursor up `rows` and set to column 1
    fn move_up_and_cr(&mut self, _: Line) {}

    /// Put `count` tabs
    fn put_tab(&mut self, _count: i64) {}

    /// Backspace `count` characters
    fn backspace(&mut self) {}

    /// Carriage return
    fn carriage_return(&mut self) {}

    /// Linefeed
    fn linefeed(&mut self) {}

    /// Linefeed and carriage return
    fn linefeed_carriage_return(&mut self) {}

    /// Ring the bell
    ///
    /// Hopefully this is never implemented
    fn bell(&mut self) {}

    /// Substitute char under cursor
    fn substitute(&mut self) {}

    /// Newline
    fn newline(&mut self) {}

    /// Set current position as a tabstop
    fn set_horizontal_tabstop(&mut self) {}

    /// Scroll up `rows` rows
    fn scroll_up(&mut self, _: Line) {}

    /// Scroll down `rows` rows
    fn scroll_down(&mut self, _: Line) {}

    /// Insert `count` blank lines
    fn insert_blank_lines(&mut self, _: Line) {}

    /// Delete `count` lines
    fn delete_lines(&mut self, _: Line) {}

    /// Erase `count` chars in current line following cursor
    ///
    /// Erase means resetting to the default state (default colors, no content,
    /// no mode flags)
    fn erase_chars(&mut self, _: Column) {}

    /// Delete `count` chars
    ///
    /// Deleting a character is like the delete key on the keyboard - everything
    /// to the right of the deleted things is shifted left.
    fn delete_chars(&mut self, _: Column) {}

    /// Move backward `count` tabs
    fn move_backward_tabs(&mut self, _count: i64) {}

    /// Move forward `count` tabs
    fn move_forward_tabs(&mut self, _count: i64) {}

    /// Save current cursor position
    fn save_cursor_position(&mut self) {}

    /// Restore cursor position
    fn restore_cursor_position(&mut self) {}

    /// Clear current line
    fn clear_line(&mut self, _mode: LineClearMode) {}

    /// Clear screen
    fn clear_screen(&mut self, _mode: ClearMode) {}

    /// Clear tab stops
    fn clear_tabs(&mut self, _mode: TabulationClearMode) {}

    /// Reset terminal state
    fn reset_state(&mut self) {}

    /// Reverse Index
    ///
    /// Move the active position to the same horizontal position on the
    /// preceding line. If the active position is at the top margin, a scroll
    /// down is performed
    fn reverse_index(&mut self) {}

    /// set a terminal attribute
    fn terminal_attribute(&mut self, _attr: Attr) {}

    /// Set mode
    fn set_mode(&mut self, _mode: Mode) {}

    /// Unset mode
    fn unset_mode(&mut self, _: Mode) {}

    /// DECSTBM - Set the terminal scrolling region
    fn set_scrolling_region(&mut self, _top: usize, _bottom: usize) {}

    /// DECKPAM - Set keypad to applications mode (ESCape instead of digits)
    fn set_keypad_application_mode(&mut self) {}

    /// DECKPNM - Set keypad to numeric mode (digits instead of ESCape seq)
    fn unset_keypad_application_mode(&mut self) {}

    /// Set one of the graphic character sets, G0 to G3, as the active charset.
    ///
    /// 'Invoke' one of G0 to G3 in the GL area. Also referred to as shift in,
    /// shift out and locking shift depending on the set being activated
    fn set_active_charset(&mut self, _: CharsetIndex) {}

    /// Assign a graphic character set to G0, G1, G2 or G3
    ///
    /// 'Designate' a graphic character set as one of G0 to G3, so that it can
    /// later be 'invoked' by `set_active_charset`
    fn configure_charset(&mut self, _: CharsetIndex, _: StandardCharset) {}

    /// Set an indexed color value
    fn set_color(&mut self, _: usize, _: Rgb) {}

    /// Write a foreground/background color escape sequence with the current color
    fn dynamic_color_sequence<W: io::Write>(&mut self, _: &mut W, _: u8, _: usize) {}

    /// Reset an indexed color to original value
    fn reset_color(&mut self, _: usize) {}

    /// Set the clipboard
    fn set_clipboard(&mut self, _: &str) {}

    /// Run the dectest routine
    fn dectest(&mut self) {}

    /// Push a title onto the stack
    fn push_title(&mut self) {}

    /// Pop the last title from the stack
    fn pop_title(&mut self) {}
}

/// Describes shape of cursor
#[derive(Debug, Eq, PartialEq, Copy, Clone, Hash, Deserialize)]
pub enum CursorStyle {
    /// Cursor is a block like `▒`
    Block,

    /// Cursor is an underscore like `_`
    Underline,

    /// Cursor is a vertical bar `⎸`
    Beam,

    /// Cursor is a box like `☐`
    HollowBlock,

    /// Invisible cursor
    Hidden,
}

impl Default for CursorStyle {
    fn default() -> CursorStyle {
        CursorStyle::Block
    }
}

/// Terminal modes
#[derive(Debug, Eq, PartialEq)]
pub enum Mode {
    /// ?1
    CursorKeys = 1,
    /// Select 80 or 132 columns per page
    ///
    /// CSI ? 3 h -> set 132 column font
    /// CSI ? 3 l -> reset 80 column font
    ///
    /// Additionally,
    ///
    /// * set margins to default positions
    /// * erases all data in page memory
    /// * resets DECLRMM to unavailable
    /// * clears data from the status line (if set to host-writable)
    DECCOLM = 3,
    /// IRM Insert Mode
    ///
    /// NB should be part of non-private mode enum
    ///
    /// * `CSI 4 h` change to insert mode
    /// * `CSI 4 l` reset to replacement mode
    Insert = 4,
    /// ?6
    Origin = 6,
    /// ?7
    LineWrap = 7,
    /// ?12
    BlinkingCursor = 12,
    /// 20
    ///
    /// NB This is actually a private mode. We should consider adding a second
    /// enumeration for public/private modesets.
    LineFeedNewLine = 20,
    /// ?25
    ShowCursor = 25,
    /// ?1000
    ReportMouseClicks = 1000,
    /// ?1002
    ReportCellMouseMotion = 1002,
    /// ?1003
    ReportAllMouseMotion = 1003,
    /// ?1004
    ReportFocusInOut = 1004,
    /// ?1006
    SgrMouse = 1006,
    /// ?1007
    AlternateScroll = 1007,
    /// ?1049
    SwapScreenAndSetRestoreCursor = 1049,
    /// ?2004
    BracketedPaste = 2004,
}

impl Mode {
    /// Create mode from a primitive
    ///
    /// TODO lots of unhandled values..
    pub fn from_primitive(intermediate: Option<&u8>, num: i64) -> Option<Mode> {
        let private = match intermediate {
            Some(b'?') => true,
            None => false,
            _ => return None,
        };

        if private {
            Some(match num {
                1 => Mode::CursorKeys,
                3 => Mode::DECCOLM,
                6 => Mode::Origin,
                7 => Mode::LineWrap,
                12 => Mode::BlinkingCursor,
                25 => Mode::ShowCursor,
                1000 => Mode::ReportMouseClicks,
                1002 => Mode::ReportCellMouseMotion,
                1003 => Mode::ReportAllMouseMotion,
                1004 => Mode::ReportFocusInOut,
                1006 => Mode::SgrMouse,
                1007 => Mode::AlternateScroll,
                1049 => Mode::SwapScreenAndSetRestoreCursor,
                2004 => Mode::BracketedPaste,
                _ => {
                    trace!("[unimplemented] primitive mode: {}", num);
                    return None;
                },
            })
        } else {
            Some(match num {
                4 => Mode::Insert,
                20 => Mode::LineFeedNewLine,
                _ => return None,
            })
        }
    }
}

/// Mode for clearing line
///
/// Relative to cursor
#[derive(Debug)]
pub enum LineClearMode {
    /// Clear right of cursor
    Right,
    /// Clear left of cursor
    Left,
    /// Clear entire line
    All,
}

/// Mode for clearing terminal
///
/// Relative to cursor
#[derive(Debug)]
pub enum ClearMode {
    /// Clear below cursor
    Below,
    /// Clear above cursor
    Above,
    /// Clear entire terminal
    All,
    /// Clear 'saved' lines (scrollback)
    Saved,
}

/// Mode for clearing tab stops
#[derive(Debug)]
pub enum TabulationClearMode {
    /// Clear stop under cursor
    Current,
    /// Clear all stops
    All,
}

/// Standard colors
///
/// The order here matters since the enum should be castable to a `usize` for
/// indexing a color list.
#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NamedColor {
    /// Black
    Black = 0,
    /// Red
    Red,
    /// Green
    Green,
    /// Yellow
    Yellow,
    /// Blue
    Blue,
    /// Magenta
    Magenta,
    /// Cyan
    Cyan,
    /// White
    White,
    /// Bright black
    BrightBlack,
    /// Bright red
    BrightRed,
    /// Bright green
    BrightGreen,
    /// Bright yellow
    BrightYellow,
    /// Bright blue
    BrightBlue,
    /// Bright magenta
    BrightMagenta,
    /// Bright cyan
    BrightCyan,
    /// Bright white
    BrightWhite,
    /// The foreground color
    Foreground = 256,
    /// The background color
    Background,
    /// Color for the cursor itself
    Cursor,
    /// Dim black
    DimBlack,
    /// Dim red
    DimRed,
    /// Dim green
    DimGreen,
    /// Dim yellow
    DimYellow,
    /// Dim blue
    DimBlue,
    /// Dim magenta
    DimMagenta,
    /// Dim cyan
    DimCyan,
    /// Dim white
    DimWhite,
    /// The bright foreground color
    BrightForeground,
    /// Dim foreground
    DimForeground,
}

impl NamedColor {
    pub fn to_bright(self) -> Self {
        match self {
            NamedColor::Foreground => NamedColor::BrightForeground,
            NamedColor::Black => NamedColor::BrightBlack,
            NamedColor::Red => NamedColor::BrightRed,
            NamedColor::Green => NamedColor::BrightGreen,
            NamedColor::Yellow => NamedColor::BrightYellow,
            NamedColor::Blue => NamedColor::BrightBlue,
            NamedColor::Magenta => NamedColor::BrightMagenta,
            NamedColor::Cyan => NamedColor::BrightCyan,
            NamedColor::White => NamedColor::BrightWhite,
            NamedColor::DimForeground => NamedColor::Foreground,
            NamedColor::DimBlack => NamedColor::Black,
            NamedColor::DimRed => NamedColor::Red,
            NamedColor::DimGreen => NamedColor::Green,
            NamedColor::DimYellow => NamedColor::Yellow,
            NamedColor::DimBlue => NamedColor::Blue,
            NamedColor::DimMagenta => NamedColor::Magenta,
            NamedColor::DimCyan => NamedColor::Cyan,
            NamedColor::DimWhite => NamedColor::White,
            val => val,
        }
    }

    pub fn to_dim(self) -> Self {
        match self {
            NamedColor::Black => NamedColor::DimBlack,
            NamedColor::Red => NamedColor::DimRed,
            NamedColor::Green => NamedColor::DimGreen,
            NamedColor::Yellow => NamedColor::DimYellow,
            NamedColor::Blue => NamedColor::DimBlue,
            NamedColor::Magenta => NamedColor::DimMagenta,
            NamedColor::Cyan => NamedColor::DimCyan,
            NamedColor::White => NamedColor::DimWhite,
            NamedColor::Foreground => NamedColor::DimForeground,
            NamedColor::BrightBlack => NamedColor::Black,
            NamedColor::BrightRed => NamedColor::Red,
            NamedColor::BrightGreen => NamedColor::Green,
            NamedColor::BrightYellow => NamedColor::Yellow,
            NamedColor::BrightBlue => NamedColor::Blue,
            NamedColor::BrightMagenta => NamedColor::Magenta,
            NamedColor::BrightCyan => NamedColor::Cyan,
            NamedColor::BrightWhite => NamedColor::White,
            NamedColor::BrightForeground => NamedColor::Foreground,
            val => val,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Named(NamedColor),
    Spec(Rgb),
    Indexed(u8),
}

/// Terminal character attributes
#[derive(Debug, Eq, PartialEq)]
pub enum Attr {
    /// Clear all special abilities
    Reset,
    /// Bold text
    Bold,
    /// Dim or secondary color
    Dim,
    /// Italic text
    Italic,
    /// Underscore text
    Underscore,
    /// Blink cursor slowly
    BlinkSlow,
    /// Blink cursor fast
    BlinkFast,
    /// Invert colors
    Reverse,
    /// Do not display characters
    Hidden,
    /// Strikeout text
    Strike,
    /// Cancel bold
    CancelBold,
    /// Cancel bold and dim
    CancelBoldDim,
    /// Cancel italic
    CancelItalic,
    /// Cancel underline
    CancelUnderline,
    /// Cancel blink
    CancelBlink,
    /// Cancel inversion
    CancelReverse,
    /// Cancel text hiding
    CancelHidden,
    /// Cancel strikeout
    CancelStrike,
    /// Set indexed foreground color
    Foreground(Color),
    /// Set indexed background color
    Background(Color),
}

/// Identifiers which can be assigned to a graphic character set
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CharsetIndex {
    /// Default set, is designated as ASCII at startup
    G0,
    G1,
    G2,
    G3,
}

impl Default for CharsetIndex {
    fn default() -> Self {
        CharsetIndex::G0
    }
}

/// Standard or common character sets which can be designated as G0-G3
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardCharset {
    Ascii,
    SpecialCharacterAndLineDrawing,
}

impl Default for StandardCharset {
    fn default() -> Self {
        StandardCharset::Ascii
    }
}

impl<'a, H, W> vte::Perform for Performer<'a, H, W>
where
    H: Handler + TermInfo + 'a,
    W: io::Write + 'a,
{
    #[inline]
    fn print(&mut self, c: char) {
        self.handler.input(c);
        self.state.preceding_char = Some(c);
    }

    #[inline]
    fn execute(&mut self, byte: u8) {
        match byte {
            C0::HT => self.handler.put_tab(1),
            C0::BS => self.handler.backspace(),
            C0::CR => self.handler.carriage_return(),
            C0::LF | C0::VT | C0::FF => self.handler.linefeed(),
            C0::BEL => self.handler.bell(),
            C0::SUB => self.handler.substitute(),
            C0::SI => self.handler.set_active_charset(CharsetIndex::G0),
            C0::SO => self.handler.set_active_charset(CharsetIndex::G1),
            C1::NEL => self.handler.newline(),
            C1::HTS => self.handler.set_horizontal_tabstop(),
            C1::DECID => self.handler.identify_terminal(self.writer),
            _ => debug!("[unhandled] execute byte={:02x}", byte),
        }
    }

    #[inline]
    fn hook(&mut self, params: &[i64], intermediates: &[u8], ignore: bool) {
        debug!(
            "[unhandled hook] params={:?}, ints: {:?}, ignore: {:?}",
            params, intermediates, ignore
        );
    }

    #[inline]
    fn put(&mut self, byte: u8) {
        debug!("[unhandled put] byte={:?}", byte);
    }

    #[inline]
    fn unhook(&mut self) {
        debug!("[unhandled unhook]");
    }

    // TODO replace OSC parsing with parser combinators
    #[inline]
    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        let writer = &mut self.writer;

        fn unhandled(params: &[&[u8]]) {
            let mut buf = String::new();
            for items in params {
                buf.push_str("[");
                for item in *items {
                    buf.push_str(&format!("{:?},", *item as char));
                }
                buf.push_str("],");
            }
            debug!("[unhandled osc_dispatch]: [{}] at line {}", &buf, line!());
        }

        if params.is_empty() || params[0].is_empty() {
            return;
        }

        match params[0] {
            // Set window title
            b"0" | b"2" => {
                if params.len() >= 2 {
                    let title = params[1..]
                        .iter()
                        .flat_map(|x| str::from_utf8(x))
                        .collect::<Vec<&str>>()
                        .join(";");
                    self.handler.set_title(&title);
                    return;
                }
                unhandled(params);
            },

            // Set icon name
            // This is ignored, since alacritty has no concept of tabs
            b"1" => (),

            // Set color index
            b"4" => {
                if params.len() > 1 && params.len() % 2 != 0 {
                    for chunk in params[1..].chunks(2) {
                        let index = parse_number(chunk[0]);
                        let color = xparse_color(chunk[1]);
                        if let (Some(i), Some(c)) = (index, color) {
                            self.handler.set_color(i as usize, c);
                            return;
                        }
                    }
                }
                unhandled(params);
            },

            // Get/set Foreground, Background, Cursor colors
            b"10" | b"11" | b"12" => {
                if params.len() >= 2 {
                    if let Some(mut dynamic_code) = parse_number(params[0]) {
                        for param in &params[1..] {
                            // 10 is the first dynamic color, also the foreground
                            let offset = dynamic_code as usize - 10;
                            let index = NamedColor::Foreground as usize + offset;

                            // End of setting dynamic colors
                            if index > NamedColor::Cursor as usize {
                                unhandled(params);
                                break;
                            }

                            if let Some(color) = xparse_color(param) {
                                self.handler.set_color(index, color);
                            } else if param == b"?" {
                                self.handler.dynamic_color_sequence(writer, dynamic_code, index);
                            } else {
                                unhandled(params);
                            }
                            dynamic_code += 1;
                        }
                        return;
                    }
                }
                unhandled(params);
            },

            // Set cursor style
            b"50" => {
                if params.len() >= 2
                    && params[1].len() >= 13
                    && params[1][0..12] == *b"CursorShape="
                {
                    let style = match params[1][12] as char {
                        '0' => CursorStyle::Block,
                        '1' => CursorStyle::Beam,
                        '2' => CursorStyle::Underline,
                        _ => return unhandled(params),
                    };
                    self.handler.set_cursor_style(Some(style));
                    return;
                }
                unhandled(params);
            },

            // Set clipboard
            b"52" => {
                if params.len() < 3 {
                    return unhandled(params);
                }

                match params[2] {
                    b"?" => unhandled(params),
                    selection => {
                        if let Ok(string) = base64::decode(selection) {
                            if let Ok(utf8_string) = str::from_utf8(&string) {
                                self.handler.set_clipboard(utf8_string);
                            }
                        }
                    },
                }
            },

            // Reset color index
            b"104" => {
                // Reset all color indexes when no parameters are given
                if params.len() == 1 {
                    for i in 0..256 {
                        self.handler.reset_color(i);
                    }
                    return;
                }

                // Reset color indexes given as parameters
                for param in &params[1..] {
                    match parse_number(param) {
                        Some(index) => self.handler.reset_color(index as usize),
                        None => unhandled(params),
                    }
                }
            },

            // Reset foreground color
            b"110" => self.handler.reset_color(NamedColor::Foreground as usize),

            // Reset background color
            b"111" => self.handler.reset_color(NamedColor::Background as usize),

            // Reset text cursor color
            b"112" => self.handler.reset_color(NamedColor::Cursor as usize),

            _ => unhandled(params),
        }
    }

    #[inline]
    fn csi_dispatch(
        &mut self,
        args: &[i64],
        intermediates: &[u8],
        has_ignored_intermediates: bool,
        action: char,
    ) {
        macro_rules! unhandled {
            () => {{
                debug!(
                    "[Unhandled CSI] action={:?}, args={:?}, intermediates={:?}",
                    action, args, intermediates
                );
            }};
        }

        macro_rules! arg_or_default {
            (idx: $idx:expr, default: $default:expr) => {
                args.get($idx)
                    .and_then(|v| if *v == 0 { None } else { Some(*v) })
                    .unwrap_or($default)
            };
        }

        if has_ignored_intermediates || intermediates.len() > 1 {
            unhandled!();
            return;
        }

        let handler = &mut self.handler;
        let writer = &mut self.writer;

        match (action, intermediates.get(0)) {
            ('@', None) => {
                handler.insert_blank(Column(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('A', None) => {
                handler.move_up(Line(arg_or_default!(idx: 0, default: 1) as usize));
            },
            ('b', None) => {
                if let Some(c) = self.state.preceding_char {
                    for _ in 0..arg_or_default!(idx: 0, default: 1) {
                        handler.input(c);
                    }
                } else {
                    debug!("tried to repeat with no preceding char");
                }
            },
            ('B', None) | ('e', None) => {
                handler.move_down(Line(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('c', None) => handler.identify_terminal(writer),
            ('C', None) | ('a', None) => {
                handler.move_forward(Column(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('D', None) => {
                handler.move_backward(Column(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('E', None) => {
                handler.move_down_and_cr(Line(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('F', None) => {
                handler.move_up_and_cr(Line(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('g', None) => {
                let mode = match arg_or_default!(idx: 0, default: 0) {
                    0 => TabulationClearMode::Current,
                    3 => TabulationClearMode::All,
                    _ => {
                        unhandled!();
                        return;
                    },
                };

                handler.clear_tabs(mode);
            },
            ('G', None) | ('`', None) => {
                handler.goto_col(Column(arg_or_default!(idx: 0, default: 1) as usize - 1))
            },
            ('H', None) | ('f', None) => {
                let y = arg_or_default!(idx: 0, default: 1) as usize;
                let x = arg_or_default!(idx: 1, default: 1) as usize;
                handler.goto(Line(y - 1), Column(x - 1));
            },
            ('I', None) => handler.move_forward_tabs(arg_or_default!(idx: 0, default: 1)),
            ('J', None) => {
                let mode = match arg_or_default!(idx: 0, default: 0) {
                    0 => ClearMode::Below,
                    1 => ClearMode::Above,
                    2 => ClearMode::All,
                    3 => ClearMode::Saved,
                    _ => {
                        unhandled!();
                        return;
                    },
                };

                handler.clear_screen(mode);
            },
            ('K', None) => {
                let mode = match arg_or_default!(idx: 0, default: 0) {
                    0 => LineClearMode::Right,
                    1 => LineClearMode::Left,
                    2 => LineClearMode::All,
                    _ => {
                        unhandled!();
                        return;
                    },
                };

                handler.clear_line(mode);
            },
            ('S', None) => handler.scroll_up(Line(arg_or_default!(idx: 0, default: 1) as usize)),
            ('t', None) => match arg_or_default!(idx: 0, default: 1) as usize {
                22 => handler.push_title(),
                23 => handler.pop_title(),
                _ => unhandled!(),
            },
            ('T', None) => handler.scroll_down(Line(arg_or_default!(idx: 0, default: 1) as usize)),
            ('L', None) => {
                handler.insert_blank_lines(Line(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('l', intermediate) => {
                for arg in args {
                    match Mode::from_primitive(intermediate, *arg) {
                        Some(mode) => handler.unset_mode(mode),
                        None => {
                            unhandled!();
                            return;
                        },
                    }
                }
            },
            ('M', None) => handler.delete_lines(Line(arg_or_default!(idx: 0, default: 1) as usize)),
            ('X', None) => {
                handler.erase_chars(Column(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('P', None) => {
                handler.delete_chars(Column(arg_or_default!(idx: 0, default: 1) as usize))
            },
            ('Z', None) => handler.move_backward_tabs(arg_or_default!(idx: 0, default: 1)),
            ('d', None) => {
                handler.goto_line(Line(arg_or_default!(idx: 0, default: 1) as usize - 1))
            },
            ('h', intermediate) => {
                for arg in args {
                    match Mode::from_primitive(intermediate, *arg) {
                        Some(mode) => handler.set_mode(mode),
                        None => {
                            unhandled!();
                            return;
                        },
                    }
                }
            },
            ('m', None) => {
                if args.is_empty() {
                    handler.terminal_attribute(Attr::Reset);
                } else {
                    for attr in attrs_from_sgr_parameters(args) {
                        match attr {
                            Some(attr) => handler.terminal_attribute(attr),
                            None => {
                                unhandled!();
                                return;
                            },
                        }
                    }
                }
            },
            ('n', None) => {
                handler.device_status(writer, arg_or_default!(idx: 0, default: 0) as usize)
            },
            ('q', Some(b' ')) => {
                // DECSCUSR (CSI Ps SP q) -- Set Cursor Style
                let style = match arg_or_default!(idx: 0, default: 0) {
                    0 => None,
                    1 | 2 => Some(CursorStyle::Block),
                    3 | 4 => Some(CursorStyle::Underline),
                    5 | 6 => Some(CursorStyle::Beam),
                    _ => {
                        unhandled!();
                        return;
                    },
                };

                handler.set_cursor_style(style);
            },
            ('r', None) => {
                let top = arg_or_default!(idx: 0, default: 1) as usize;
                let bottom = arg_or_default!(idx: 1, default: handler.lines().0 as _) as usize;

                handler.set_scrolling_region(top, bottom);
            },
            ('s', None) => handler.save_cursor_position(),
            ('u', None) => handler.restore_cursor_position(),
            _ => unhandled!(),
        }
    }

    #[inline]
    fn esc_dispatch(&mut self, params: &[i64], intermediates: &[u8], _ignore: bool, byte: u8) {
        macro_rules! unhandled {
            () => {{
                debug!(
                    "[unhandled] esc_dispatch params={:?}, ints={:?}, byte={:?} ({:02x})",
                    params, intermediates, byte as char, byte
                );
            }};
        }

        macro_rules! configure_charset {
            ($charset:path) => {{
                let index: CharsetIndex = match intermediates.first().cloned() {
                    Some(b'(') => CharsetIndex::G0,
                    Some(b')') => CharsetIndex::G1,
                    Some(b'*') => CharsetIndex::G2,
                    Some(b'+') => CharsetIndex::G3,
                    _ => {
                        unhandled!();
                        return;
                    },
                };
                self.handler.configure_charset(index, $charset)
            }};
        }

        match byte {
            b'B' => configure_charset!(StandardCharset::Ascii),
            b'D' => self.handler.linefeed(),
            b'E' => {
                self.handler.linefeed();
                self.handler.carriage_return();
            },
            b'H' => self.handler.set_horizontal_tabstop(),
            b'M' => self.handler.reverse_index(),
            b'Z' => self.handler.identify_terminal(self.writer),
            b'c' => self.handler.reset_state(),
            b'0' => configure_charset!(StandardCharset::SpecialCharacterAndLineDrawing),
            b'7' => self.handler.save_cursor_position(),
            b'8' => {
                if !intermediates.is_empty() && intermediates[0] == b'#' {
                    self.handler.dectest();
                } else {
                    self.handler.restore_cursor_position();
                }
            },
            b'=' => self.handler.set_keypad_application_mode(),
            b'>' => self.handler.unset_keypad_application_mode(),
            b'\\' => (), // String terminator, do nothing (parser handles as string terminator)
            _ => unhandled!(),
        }
    }
}

fn attrs_from_sgr_parameters(parameters: &[i64]) -> Vec<Option<Attr>> {
    // Sometimes a C-style for loop is just what you need
    let mut i = 0; // C-for initializer
    let mut attrs = Vec::with_capacity(parameters.len());
    loop {
        if i >= parameters.len() {
            // C-for condition
            break;
        }

        let attr = match parameters[i] {
            0 => Some(Attr::Reset),
            1 => Some(Attr::Bold),
            2 => Some(Attr::Dim),
            3 => Some(Attr::Italic),
            4 => Some(Attr::Underscore),
            5 => Some(Attr::BlinkSlow),
            6 => Some(Attr::BlinkFast),
            7 => Some(Attr::Reverse),
            8 => Some(Attr::Hidden),
            9 => Some(Attr::Strike),
            21 => Some(Attr::CancelBold),
            22 => Some(Attr::CancelBoldDim),
            23 => Some(Attr::CancelItalic),
            24 => Some(Attr::CancelUnderline),
            25 => Some(Attr::CancelBlink),
            27 => Some(Attr::CancelReverse),
            28 => Some(Attr::CancelHidden),
            29 => Some(Attr::CancelStrike),
            30 => Some(Attr::Foreground(Color::Named(NamedColor::Black))),
            31 => Some(Attr::Foreground(Color::Named(NamedColor::Red))),
            32 => Some(Attr::Foreground(Color::Named(NamedColor::Green))),
            33 => Some(Attr::Foreground(Color::Named(NamedColor::Yellow))),
            34 => Some(Attr::Foreground(Color::Named(NamedColor::Blue))),
            35 => Some(Attr::Foreground(Color::Named(NamedColor::Magenta))),
            36 => Some(Attr::Foreground(Color::Named(NamedColor::Cyan))),
            37 => Some(Attr::Foreground(Color::Named(NamedColor::White))),
            38 => {
                let mut start = 0;
                if let Some(color) = parse_sgr_color(&parameters[i..], &mut start) {
                    i += start;
                    Some(Attr::Foreground(color))
                } else {
                    None
                }
            },
            39 => Some(Attr::Foreground(Color::Named(NamedColor::Foreground))),
            40 => Some(Attr::Background(Color::Named(NamedColor::Black))),
            41 => Some(Attr::Background(Color::Named(NamedColor::Red))),
            42 => Some(Attr::Background(Color::Named(NamedColor::Green))),
            43 => Some(Attr::Background(Color::Named(NamedColor::Yellow))),
            44 => Some(Attr::Background(Color::Named(NamedColor::Blue))),
            45 => Some(Attr::Background(Color::Named(NamedColor::Magenta))),
            46 => Some(Attr::Background(Color::Named(NamedColor::Cyan))),
            47 => Some(Attr::Background(Color::Named(NamedColor::White))),
            48 => {
                let mut start = 0;
                if let Some(color) = parse_sgr_color(&parameters[i..], &mut start) {
                    i += start;
                    Some(Attr::Background(color))
                } else {
                    None
                }
            },
            49 => Some(Attr::Background(Color::Named(NamedColor::Background))),
            90 => Some(Attr::Foreground(Color::Named(NamedColor::BrightBlack))),
            91 => Some(Attr::Foreground(Color::Named(NamedColor::BrightRed))),
            92 => Some(Attr::Foreground(Color::Named(NamedColor::BrightGreen))),
            93 => Some(Attr::Foreground(Color::Named(NamedColor::BrightYellow))),
            94 => Some(Attr::Foreground(Color::Named(NamedColor::BrightBlue))),
            95 => Some(Attr::Foreground(Color::Named(NamedColor::BrightMagenta))),
            96 => Some(Attr::Foreground(Color::Named(NamedColor::BrightCyan))),
            97 => Some(Attr::Foreground(Color::Named(NamedColor::BrightWhite))),
            100 => Some(Attr::Background(Color::Named(NamedColor::BrightBlack))),
            101 => Some(Attr::Background(Color::Named(NamedColor::BrightRed))),
            102 => Some(Attr::Background(Color::Named(NamedColor::BrightGreen))),
            103 => Some(Attr::Background(Color::Named(NamedColor::BrightYellow))),
            104 => Some(Attr::Background(Color::Named(NamedColor::BrightBlue))),
            105 => Some(Attr::Background(Color::Named(NamedColor::BrightMagenta))),
            106 => Some(Attr::Background(Color::Named(NamedColor::BrightCyan))),
            107 => Some(Attr::Background(Color::Named(NamedColor::BrightWhite))),
            _ => None,
        };

        attrs.push(attr);

        i += 1; // C-for expr
    }
    attrs
}

/// Parse a color specifier from list of attributes
fn parse_sgr_color(attrs: &[i64], i: &mut usize) -> Option<Color> {
    if attrs.len() < 2 {
        return None;
    }

    match attrs[*i + 1] {
        2 => {
            // RGB color spec
            if attrs.len() < 5 {
                debug!("Expected RGB color spec; got {:?}", attrs);
                return None;
            }

            let r = attrs[*i + 2];
            let g = attrs[*i + 3];
            let b = attrs[*i + 4];

            *i += 4;

            let range = 0..256;
            if !range.contains_(r) || !range.contains_(g) || !range.contains_(b) {
                debug!("Invalid RGB color spec: ({}, {}, {})", r, g, b);
                return None;
            }

            Some(Color::Spec(Rgb { r: r as u8, g: g as u8, b: b as u8 }))
        },
        5 => {
            if attrs.len() < 3 {
                debug!("Expected color index; got {:?}", attrs);
                None
            } else {
                *i += 2;
                let idx = attrs[*i];
                match idx {
                    0..=255 => Some(Color::Indexed(idx as u8)),
                    _ => {
                        debug!("Invalid color index: {}", idx);
                        None
                    },
                }
            }
        },
        _ => {
            debug!("Unexpected color attr: {}", attrs[*i + 1]);
            None
        },
    }
}

/// C0 set of 7-bit control characters (from ANSI X3.4-1977).
#[allow(non_snake_case)]
pub mod C0 {
    /// Null filler, terminal should ignore this character
    pub const NUL: u8 = 0x00;
    /// Start of Header
    pub const SOH: u8 = 0x01;
    /// Start of Text, implied end of header
    pub const STX: u8 = 0x02;
    /// End of Text, causes some terminal to respond with ACK or NAK
    pub const ETX: u8 = 0x03;
    /// End of Transmission
    pub const EOT: u8 = 0x04;
    /// Enquiry, causes terminal to send ANSWER-BACK ID
    pub const ENQ: u8 = 0x05;
    /// Acknowledge, usually sent by terminal in response to ETX
    pub const ACK: u8 = 0x06;
    /// Bell, triggers the bell, buzzer, or beeper on the terminal
    pub const BEL: u8 = 0x07;
    /// Backspace, can be used to define overstruck characters
    pub const BS: u8 = 0x08;
    /// Horizontal Tabulation, move to next predetermined position
    pub const HT: u8 = 0x09;
    /// Linefeed, move to same position on next line (see also NL)
    pub const LF: u8 = 0x0A;
    /// Vertical Tabulation, move to next predetermined line
    pub const VT: u8 = 0x0B;
    /// Form Feed, move to next form or page
    pub const FF: u8 = 0x0C;
    /// Carriage Return, move to first character of current line
    pub const CR: u8 = 0x0D;
    /// Shift Out, switch to G1 (other half of character set)
    pub const SO: u8 = 0x0E;
    /// Shift In, switch to G0 (normal half of character set)
    pub const SI: u8 = 0x0F;
    /// Data Link Escape, interpret next control character specially
    pub const DLE: u8 = 0x10;
    /// (DC1) Terminal is allowed to resume transmitting
    pub const XON: u8 = 0x11;
    /// Device Control 2, causes ASR-33 to activate paper-tape reader
    pub const DC2: u8 = 0x12;
    /// (DC2) Terminal must pause and refrain from transmitting
    pub const XOFF: u8 = 0x13;
    /// Device Control 4, causes ASR-33 to deactivate paper-tape reader
    pub const DC4: u8 = 0x14;
    /// Negative Acknowledge, used sometimes with ETX and ACK
    pub const NAK: u8 = 0x15;
    /// Synchronous Idle, used to maintain timing in Sync communication
    pub const SYN: u8 = 0x16;
    /// End of Transmission block
    pub const ETB: u8 = 0x17;
    /// Cancel (makes VT100 abort current escape sequence if any)
    pub const CAN: u8 = 0x18;
    /// End of Medium
    pub const EM: u8 = 0x19;
    /// Substitute (VT100 uses this to display parity errors)
    pub const SUB: u8 = 0x1A;
    /// Prefix to an escape sequence
    pub const ESC: u8 = 0x1B;
    /// File Separator
    pub const FS: u8 = 0x1C;
    /// Group Separator
    pub const GS: u8 = 0x1D;
    /// Record Separator (sent by VT132 in block-transfer mode)
    pub const RS: u8 = 0x1E;
    /// Unit Separator
    pub const US: u8 = 0x1F;
    /// Delete, should be ignored by terminal
    pub const DEL: u8 = 0x7f;
}

/// C1 set of 8-bit control characters (from ANSI X3.64-1979)
///
/// 0x80 (@), 0x81 (A), 0x82 (B), 0x83 (C) are reserved
/// 0x98 (X), 0x99 (Y) are reserved
/// 0x9a (Z) is 'reserved', but causes DEC terminals to respond with DA codes
#[allow(non_snake_case)]
pub mod C1 {
    /// Reserved
    pub const PAD: u8 = 0x80;
    /// Reserved
    pub const HOP: u8 = 0x81;
    /// Reserved
    pub const BPH: u8 = 0x82;
    /// Reserved
    pub const NBH: u8 = 0x83;
    /// Index, moves down one line same column regardless of NL
    pub const IND: u8 = 0x84;
    /// New line, moves done one line and to first column (CR+LF)
    pub const NEL: u8 = 0x85;
    /// Start of Selected Area to be sent to auxiliary output device
    pub const SSA: u8 = 0x86;
    /// End of Selected Area to be sent to auxiliary output device
    pub const ESA: u8 = 0x87;
    /// Horizontal Tabulation Set at current position
    pub const HTS: u8 = 0x88;
    /// Hor Tab Justify, moves string to next tab position
    pub const HTJ: u8 = 0x89;
    /// Vertical Tabulation Set at current line
    pub const VTS: u8 = 0x8A;
    /// Partial Line Down (subscript)
    pub const PLD: u8 = 0x8B;
    /// Partial Line Up (superscript)
    pub const PLU: u8 = 0x8C;
    /// Reverse Index, go up one line, reverse scroll if necessary
    pub const RI: u8 = 0x8D;
    /// Single Shift to G2
    pub const SS2: u8 = 0x8E;
    /// Single Shift to G3 (VT100 uses this for sending PF keys)
    pub const SS3: u8 = 0x8F;
    /// Device Control String, terminated by ST (VT125 enters graphics)
    pub const DCS: u8 = 0x90;
    /// Private Use 1
    pub const PU1: u8 = 0x91;
    /// Private Use 2
    pub const PU2: u8 = 0x92;
    /// Set Transmit State
    pub const STS: u8 = 0x93;
    /// Cancel character, ignore previous character
    pub const CCH: u8 = 0x94;
    /// Message Waiting, turns on an indicator on the terminal
    pub const MW: u8 = 0x95;
    /// Start of Protected Area
    pub const SPA: u8 = 0x96;
    /// End of Protected Area
    pub const EPA: u8 = 0x97;
    /// SOS
    pub const SOS: u8 = 0x98;
    /// SGCI
    pub const SGCI: u8 = 0x99;
    /// DECID - Identify Terminal
    pub const DECID: u8 = 0x9a;
    /// Control Sequence Introducer
    pub const CSI: u8 = 0x9B;
    /// String Terminator (VT125 exits graphics)
    pub const ST: u8 = 0x9C;
    /// Operating System Command (reprograms intelligent terminal)
    pub const OSC: u8 = 0x9D;
    /// Privacy Message (password verification), terminated by ST
    pub const PM: u8 = 0x9E;
    /// Application Program Command (to word processor), term by ST
    pub const APC: u8 = 0x9F;
}

// Tests for parsing escape sequences
//
// Byte sequences used in these tests are recording of pty stdout.
#[cfg(test)]
mod tests {
    use super::{
        parse_number, xparse_color, Attr, CharsetIndex, Color, Handler, Processor, StandardCharset,
        TermInfo,
    };
    use crate::index::{Column, Line};
    use crate::term::color::Rgb;
    use std::io;

    /// The /dev/null of `io::Write`
    struct Void;

    impl io::Write for Void {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct AttrHandler {
        attr: Option<Attr>,
    }

    impl Handler for AttrHandler {
        fn terminal_attribute(&mut self, attr: Attr) {
            self.attr = Some(attr);
        }
    }

    impl TermInfo for AttrHandler {
        fn lines(&self) -> Line {
            Line(24)
        }

        fn cols(&self) -> Column {
            Column(80)
        }
    }

    #[test]
    fn parse_control_attribute() {
        static BYTES: &[u8] = &[0x1b, 0x5b, 0x31, 0x6d];

        let mut parser = Processor::new();
        let mut handler = AttrHandler::default();

        for byte in &BYTES[..] {
            parser.advance(&mut handler, *byte, &mut Void);
        }

        assert_eq!(handler.attr, Some(Attr::Bold));
    }

    #[test]
    fn parse_truecolor_attr() {
        static BYTES: &[u8] = &[
            0x1b, 0x5b, 0x33, 0x38, 0x3b, 0x32, 0x3b, 0x31, 0x32, 0x38, 0x3b, 0x36, 0x36, 0x3b,
            0x32, 0x35, 0x35, 0x6d,
        ];

        let mut parser = Processor::new();
        let mut handler = AttrHandler::default();

        for byte in &BYTES[..] {
            parser.advance(&mut handler, *byte, &mut Void);
        }

        let spec = Rgb { r: 128, g: 66, b: 255 };

        assert_eq!(handler.attr, Some(Attr::Foreground(Color::Spec(spec))));
    }

    /// No exactly a test; useful for debugging
    #[test]
    fn parse_zsh_startup() {
        static BYTES: &[u8] = &[
            0x1b, 0x5b, 0x31, 0x6d, 0x1b, 0x5b, 0x37, 0x6d, 0x25, 0x1b, 0x5b, 0x32, 0x37, 0x6d,
            0x1b, 0x5b, 0x31, 0x6d, 0x1b, 0x5b, 0x30, 0x6d, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
            0x20, 0x20, 0x20, 0x0d, 0x20, 0x0d, 0x0d, 0x1b, 0x5b, 0x30, 0x6d, 0x1b, 0x5b, 0x32,
            0x37, 0x6d, 0x1b, 0x5b, 0x32, 0x34, 0x6d, 0x1b, 0x5b, 0x4a, 0x6a, 0x77, 0x69, 0x6c,
            0x6d, 0x40, 0x6a, 0x77, 0x69, 0x6c, 0x6d, 0x2d, 0x64, 0x65, 0x73, 0x6b, 0x20, 0x1b,
            0x5b, 0x30, 0x31, 0x3b, 0x33, 0x32, 0x6d, 0xe2, 0x9e, 0x9c, 0x20, 0x1b, 0x5b, 0x30,
            0x31, 0x3b, 0x33, 0x32, 0x6d, 0x20, 0x1b, 0x5b, 0x33, 0x36, 0x6d, 0x7e, 0x2f, 0x63,
            0x6f, 0x64, 0x65,
        ];

        let mut handler = AttrHandler::default();
        let mut parser = Processor::new();

        for byte in &BYTES[..] {
            parser.advance(&mut handler, *byte, &mut Void);
        }
    }

    struct CharsetHandler {
        index: CharsetIndex,
        charset: StandardCharset,
    }

    impl Default for CharsetHandler {
        fn default() -> CharsetHandler {
            CharsetHandler { index: CharsetIndex::G0, charset: StandardCharset::Ascii }
        }
    }

    impl Handler for CharsetHandler {
        fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
            self.index = index;
            self.charset = charset;
        }

        fn set_active_charset(&mut self, index: CharsetIndex) {
            self.index = index;
        }
    }

    impl TermInfo for CharsetHandler {
        fn lines(&self) -> Line {
            Line(200)
        }

        fn cols(&self) -> Column {
            Column(90)
        }
    }

    #[test]
    fn parse_designate_g0_as_line_drawing() {
        static BYTES: &[u8] = &[0x1b, b'(', b'0'];
        let mut parser = Processor::new();
        let mut handler = CharsetHandler::default();

        for byte in &BYTES[..] {
            parser.advance(&mut handler, *byte, &mut Void);
        }

        assert_eq!(handler.index, CharsetIndex::G0);
        assert_eq!(handler.charset, StandardCharset::SpecialCharacterAndLineDrawing);
    }

    #[test]
    fn parse_designate_g1_as_line_drawing_and_invoke() {
        static BYTES: &[u8] = &[0x1b, 0x29, 0x30, 0x0e];
        let mut parser = Processor::new();
        let mut handler = CharsetHandler::default();

        for byte in &BYTES[..3] {
            parser.advance(&mut handler, *byte, &mut Void);
        }

        assert_eq!(handler.index, CharsetIndex::G1);
        assert_eq!(handler.charset, StandardCharset::SpecialCharacterAndLineDrawing);

        let mut handler = CharsetHandler::default();
        parser.advance(&mut handler, BYTES[3], &mut Void);

        assert_eq!(handler.index, CharsetIndex::G1);
    }

    #[test]
    fn parse_valid_rgb_colors() {
        assert_eq!(xparse_color(b"rgb:f/e/d"), Some(Rgb { r: 0xff, g: 0xee, b: 0xdd }));
        assert_eq!(xparse_color(b"rgb:11/aa/ff"), Some(Rgb { r: 0x11, g: 0xaa, b: 0xff }));
        assert_eq!(xparse_color(b"rgb:f/ed1/cb23"), Some(Rgb { r: 0xff, g: 0xec, b: 0xca }));
        assert_eq!(xparse_color(b"rgb:ffff/0/0"), Some(Rgb { r: 0xff, g: 0x0, b: 0x0 }));
    }

    #[test]
    fn parse_valid_legacy_rgb_colors() {
        assert_eq!(xparse_color(b"#1af"), Some(Rgb { r: 0x10, g: 0xa0, b: 0xf0 }));
        assert_eq!(xparse_color(b"#11aaff"), Some(Rgb { r: 0x11, g: 0xaa, b: 0xff }));
        assert_eq!(xparse_color(b"#110aa0ff0"), Some(Rgb { r: 0x11, g: 0xaa, b: 0xff }));
        assert_eq!(xparse_color(b"#1100aa00ff00"), Some(Rgb { r: 0x11, g: 0xaa, b: 0xff }));
    }

    #[test]
    fn parse_invalid_rgb_colors() {
        assert_eq!(xparse_color(b"rgb:0//"), None);
        assert_eq!(xparse_color(b"rgb://///"), None);
    }

    #[test]
    fn parse_invalid_legacy_rgb_colors() {
        assert_eq!(xparse_color(b"#"), None);
        assert_eq!(xparse_color(b"#f"), None);
    }

    #[test]
    fn parse_invalid_number() {
        assert_eq!(parse_number(b"1abc"), None);
    }

    #[test]
    fn parse_valid_number() {
        assert_eq!(parse_number(b"123"), Some(123));
    }

    #[test]
    fn parse_number_too_large() {
        assert_eq!(parse_number(b"321"), None);
    }
}
