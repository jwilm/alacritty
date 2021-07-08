//! ANSI Terminal Stream Parsing.

use std::convert::TryFrom;
use std::time::{Duration, Instant};
use std::{iter, str};

use log::{debug, trace};
use serde::{Deserialize, Serialize};
use vte::{Params, ParamsIter};

use alacritty_config_derive::ConfigDeserialize;

use crate::graphics::{sixel, GraphicData};
use crate::index::{Column, Line};
use crate::term::color::Rgb;

/// Maximum time before a synchronized update is aborted.
const SYNC_UPDATE_TIMEOUT: Duration = Duration::from_millis(150);

/// Maximum number of bytes read in one synchronized update (2MiB).
const SYNC_BUFFER_SIZE: usize = 0x20_0000;

/// Number of bytes in the synchronized update DCS sequence before the passthrough parameters.
const SYNC_ESCAPE_START_LEN: usize = 5;

/// Start of the DCS sequence for beginning synchronized updates.
const SYNC_START_ESCAPE_START: [u8; SYNC_ESCAPE_START_LEN] = [b'\x1b', b'P', b'=', b'1', b's'];

/// Start of the DCS sequence for terminating synchronized updates.
const SYNC_END_ESCAPE_START: [u8; SYNC_ESCAPE_START_LEN] = [b'\x1b', b'P', b'=', b'2', b's'];

/// Parse colors in XParseColor format.
fn xparse_color(color: &[u8]) -> Option<Rgb> {
    if !color.is_empty() && color[0] == b'#' {
        parse_legacy_color(&color[1..])
    } else if color.len() >= 4 && &color[..4] == b"rgb:" {
        parse_rgb_color(&color[4..])
    } else {
        None
    }
}

/// Parse colors in `rgb:r(rrr)/g(ggg)/b(bbb)` format.
fn parse_rgb_color(color: &[u8]) -> Option<Rgb> {
    let colors = str::from_utf8(color).ok()?.split('/').collect::<Vec<_>>();

    if colors.len() != 3 {
        return None;
    }

    // Scale values instead of filling with `0`s.
    let scale = |input: &str| {
        if input.len() > 4 {
            None
        } else {
            let max = u32::pow(16, input.len() as u32) - 1;
            let value = u32::from_str_radix(input, 16).ok()?;
            Some((255 * value / max) as u8)
        }
    };

    Some(Rgb { r: scale(colors[0])?, g: scale(colors[1])?, b: scale(colors[2])? })
}

/// Parse colors in `#r(rrr)g(ggg)b(bbb)` format.
fn parse_legacy_color(color: &[u8]) -> Option<Rgb> {
    let item_len = color.len() / 3;

    // Truncate/Fill to two byte precision.
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

/// Internal state for VTE processor.
#[derive(Debug, Default)]
struct ProcessorState {
    /// Last processed character for repetition.
    preceding_char: Option<char>,

    /// DCS sequence waiting for termination.
    dcs: Option<Dcs>,

    /// State for synchronized terminal updates.
    sync_state: SyncState,
}

#[derive(Debug)]
struct SyncState {
    /// Expiration time of the synchronized update.
    timeout: Option<Instant>,

    /// Sync DCS waiting for termination sequence.
    pending_dcs: Option<Dcs>,

    /// Bytes read during the synchronized update.
    buffer: Vec<u8>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self { buffer: Vec::with_capacity(SYNC_BUFFER_SIZE), pending_dcs: None, timeout: None }
    }
}

/// Pending DCS sequence.
#[derive(Debug)]
enum Dcs {
    /// Begin of the synchronized update.
    SyncStart,

    /// End of the synchronized update.
    SyncEnd,

    /// Sixel data
    SixelData(Box<sixel::Parser>),
}

/// The processor wraps a `vte::Parser` to ultimately call methods on a Handler.
#[derive(Default)]
pub struct Processor {
    state: ProcessorState,
    parser: vte::Parser,
}

impl Processor {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a new byte from the PTY.
    #[inline]
    pub fn advance<H>(&mut self, handler: &mut H, byte: u8)
    where
        H: Handler,
    {
        if self.state.sync_state.timeout.is_none() {
            let mut performer = Performer::new(&mut self.state, handler);
            self.parser.advance(&mut performer, byte);
        } else {
            self.advance_sync(handler, byte);
        }
    }

    /// End a synchronized update.
    pub fn stop_sync<H>(&mut self, handler: &mut H)
    where
        H: Handler,
    {
        // Process all synchronized bytes.
        for i in 0..self.state.sync_state.buffer.len() {
            let byte = self.state.sync_state.buffer[i];
            let mut performer = Performer::new(&mut self.state, handler);
            self.parser.advance(&mut performer, byte);
        }

        // Resetting state after processing makes sure we don't interpret buffered sync escapes.
        self.state.sync_state.buffer.clear();
        self.state.sync_state.timeout = None;
    }

    /// Synchronized update expiration time.
    #[inline]
    pub fn sync_timeout(&self) -> Option<&Instant> {
        self.state.sync_state.timeout.as_ref()
    }

    /// Number of bytes in the synchronization buffer.
    #[inline]
    pub fn sync_bytes_count(&self) -> usize {
        self.state.sync_state.buffer.len()
    }

    /// Process a new byte during a synchronized update.
    #[cold]
    fn advance_sync<H>(&mut self, handler: &mut H, byte: u8)
    where
        H: Handler,
    {
        self.state.sync_state.buffer.push(byte);

        // Handle sync DCS escape sequences.
        match self.state.sync_state.pending_dcs {
            Some(_) => self.advance_sync_dcs_end(handler, byte),
            None => self.advance_sync_dcs_start(),
        }
    }

    /// Find the start of sync DCS sequences.
    fn advance_sync_dcs_start(&mut self) {
        // Get the last few bytes for comparison.
        let len = self.state.sync_state.buffer.len();
        let offset = len.saturating_sub(SYNC_ESCAPE_START_LEN);
        let end = &self.state.sync_state.buffer[offset..];

        // Check for extension/termination of the synchronized update.
        if end == SYNC_START_ESCAPE_START {
            self.state.sync_state.pending_dcs = Some(Dcs::SyncStart);
        } else if end == SYNC_END_ESCAPE_START || len >= SYNC_BUFFER_SIZE - 1 {
            self.state.sync_state.pending_dcs = Some(Dcs::SyncEnd);
        }
    }

    /// Parse the DCS termination sequence for synchronized updates.
    fn advance_sync_dcs_end<H>(&mut self, handler: &mut H, byte: u8)
    where
        H: Handler,
    {
        match byte {
            // Ignore DCS passthrough characters.
            0x00..=0x17 | 0x19 | 0x1c..=0x7f | 0xa0..=0xff => (),
            // Cancel the DCS sequence.
            0x18 | 0x1a | 0x80..=0x9f => self.state.sync_state.pending_dcs = None,
            // Dispatch on ESC.
            0x1b => match self.state.sync_state.pending_dcs.take() {
                Some(Dcs::SyncStart) => {
                    self.state.sync_state.timeout = Some(Instant::now() + SYNC_UPDATE_TIMEOUT);
                },
                Some(Dcs::SyncEnd) => self.stop_sync(handler),
                Some(Dcs::SixelData(_)) => (),
                None => (),
            },
        }
    }
}

/// Helper type that implements `vte::Perform`.
///
/// Processor creates a Performer when running advance and passes the Performer
/// to `vte::Parser`.
struct Performer<'a, H: Handler> {
    state: &'a mut ProcessorState,
    handler: &'a mut H,
}

impl<'a, H: Handler + 'a> Performer<'a, H> {
    /// Create a performer.
    #[inline]
    pub fn new<'b>(state: &'b mut ProcessorState, handler: &'b mut H) -> Performer<'b, H> {
        Performer { state, handler }
    }
}

/// Type that handles actions from the parser.
///
/// XXX Should probably not provide default impls for everything, but it makes
/// writing specific handler impls for tests far easier.
pub trait Handler {
    /// OSC to set window title.
    fn set_title(&mut self, _: Option<String>) {}

    /// Set the cursor style.
    fn set_cursor_style(&mut self, _: Option<CursorStyle>) {}

    /// Set the cursor shape.
    fn set_cursor_shape(&mut self, _shape: CursorShape) {}

    /// A character to be displayed.
    fn input(&mut self, _c: char) {}

    /// Set cursor to position.
    fn goto(&mut self, _: Line, _: Column) {}

    /// Set cursor to specific row.
    fn goto_line(&mut self, _: Line) {}

    /// Set cursor to specific column.
    fn goto_col(&mut self, _: Column) {}

    /// Insert blank characters in current line starting from cursor.
    fn insert_blank(&mut self, _: usize) {}

    /// Move cursor up `rows`.
    fn move_up(&mut self, _: usize) {}

    /// Move cursor down `rows`.
    fn move_down(&mut self, _: usize) {}

    /// Identify the terminal (should write back to the pty stream).
    fn identify_terminal(&mut self, _intermediate: Option<char>) {}

    /// Report device status.
    fn device_status(&mut self, _: usize) {}

    /// Move cursor forward `cols`.
    fn move_forward(&mut self, _: Column) {}

    /// Move cursor backward `cols`.
    fn move_backward(&mut self, _: Column) {}

    /// Move cursor down `rows` and set to column 1.
    fn move_down_and_cr(&mut self, _: usize) {}

    /// Move cursor up `rows` and set to column 1.
    fn move_up_and_cr(&mut self, _: usize) {}

    /// Put `count` tabs.
    fn put_tab(&mut self, _count: u16) {}

    /// Backspace `count` characters.
    fn backspace(&mut self) {}

    /// Carriage return.
    fn carriage_return(&mut self) {}

    /// Linefeed.
    fn linefeed(&mut self) {}

    /// Ring the bell.
    ///
    /// Hopefully this is never implemented.
    fn bell(&mut self) {}

    /// Substitute char under cursor.
    fn substitute(&mut self) {}

    /// Newline.
    fn newline(&mut self) {}

    /// Set current position as a tabstop.
    fn set_horizontal_tabstop(&mut self) {}

    /// Scroll up `rows` rows.
    fn scroll_up(&mut self, _: usize) {}

    /// Scroll down `rows` rows.
    fn scroll_down(&mut self, _: usize) {}

    /// Insert `count` blank lines.
    fn insert_blank_lines(&mut self, _: usize) {}

    /// Delete `count` lines.
    fn delete_lines(&mut self, _: usize) {}

    /// Erase `count` chars in current line following cursor.
    ///
    /// Erase means resetting to the default state (default colors, no content,
    /// no mode flags).
    fn erase_chars(&mut self, _: Column) {}

    /// Delete `count` chars.
    ///
    /// Deleting a character is like the delete key on the keyboard - everything
    /// to the right of the deleted things is shifted left.
    fn delete_chars(&mut self, _: usize) {}

    /// Move backward `count` tabs.
    fn move_backward_tabs(&mut self, _count: u16) {}

    /// Move forward `count` tabs.
    fn move_forward_tabs(&mut self, _count: u16) {}

    /// Save current cursor position.
    fn save_cursor_position(&mut self) {}

    /// Restore cursor position.
    fn restore_cursor_position(&mut self) {}

    /// Clear current line.
    fn clear_line(&mut self, _mode: LineClearMode) {}

    /// Clear screen.
    fn clear_screen(&mut self, _mode: ClearMode) {}

    /// Clear tab stops.
    fn clear_tabs(&mut self, _mode: TabulationClearMode) {}

    /// Reset terminal state.
    fn reset_state(&mut self) {}

    /// Reverse Index.
    ///
    /// Move the active position to the same horizontal position on the
    /// preceding line. If the active position is at the top margin, a scroll
    /// down is performed.
    fn reverse_index(&mut self) {}

    /// Set a terminal attribute.
    fn terminal_attribute(&mut self, _attr: Attr) {}

    /// Set mode.
    fn set_mode(&mut self, _mode: Mode) {}

    /// Unset mode.
    fn unset_mode(&mut self, _: Mode) {}

    /// DECSTBM - Set the terminal scrolling region.
    fn set_scrolling_region(&mut self, _top: usize, _bottom: Option<usize>) {}

    /// DECKPAM - Set keypad to applications mode (ESCape instead of digits).
    fn set_keypad_application_mode(&mut self) {}

    /// DECKPNM - Set keypad to numeric mode (digits instead of ESCape seq).
    fn unset_keypad_application_mode(&mut self) {}

    /// Set one of the graphic character sets, G0 to G3, as the active charset.
    ///
    /// 'Invoke' one of G0 to G3 in the GL area. Also referred to as shift in,
    /// shift out and locking shift depending on the set being activated.
    fn set_active_charset(&mut self, _: CharsetIndex) {}

    /// Assign a graphic character set to G0, G1, G2 or G3.
    ///
    /// 'Designate' a graphic character set as one of G0 to G3, so that it can
    /// later be 'invoked' by `set_active_charset`.
    fn configure_charset(&mut self, _: CharsetIndex, _: StandardCharset) {}

    /// Set an indexed color value.
    fn set_color(&mut self, _: usize, _: Rgb) {}

    /// Write a foreground/background color escape sequence with the current color.
    fn dynamic_color_sequence(&mut self, _: u8, _: usize, _: &str) {}

    /// Reset an indexed color to original value.
    fn reset_color(&mut self, _: usize) {}

    /// Store data into clipboard.
    fn clipboard_store(&mut self, _: u8, _: &[u8]) {}

    /// Load data from clipboard.
    fn clipboard_load(&mut self, _: u8, _: &str) {}

    /// Run the decaln routine.
    fn decaln(&mut self) {}

    /// Push a title onto the stack.
    fn push_title(&mut self) {}

    /// Pop the last title from the stack.
    fn pop_title(&mut self) {}

    /// Report text area size in pixels.
    fn text_area_size_pixels(&mut self) {}

    /// Report text area size in characters.
    fn text_area_size_chars(&mut self) {}

    /// Report a graphics attribute.
    fn graphics_attribute(&mut self, _: u16, _: u16) {}

    /// Create a parser for Sixel data.
    fn start_sixel_graphic(&mut self, _params: &Params) -> Option<Box<sixel::Parser>> {
        None
    }

    /// Insert a new graphic item.
    fn insert_graphic(&mut self, _data: GraphicData, _palette: Option<Vec<Rgb>>) {}
}

/// Terminal cursor configuration.
#[derive(ConfigDeserialize, Default, Debug, Eq, PartialEq, Copy, Clone, Hash)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

/// Terminal cursor shape.
#[derive(ConfigDeserialize, Debug, Eq, PartialEq, Copy, Clone, Hash)]
pub enum CursorShape {
    /// Cursor is a block like `▒`.
    Block,

    /// Cursor is an underscore like `_`.
    Underline,

    /// Cursor is a vertical bar `⎸`.
    Beam,

    /// Cursor is a box like `☐`.
    #[config(skip)]
    HollowBlock,

    /// Invisible cursor.
    #[config(skip)]
    Hidden,
}

impl Default for CursorShape {
    fn default() -> CursorShape {
        CursorShape::Block
    }
}

/// Terminal modes.
#[derive(Debug, Eq, PartialEq)]
pub enum Mode {
    /// ?1
    CursorKeys = 1,
    /// Select 80 or 132 columns per page (DECCOLM).
    ///
    /// CSI ? 3 h -> set 132 column font.
    /// CSI ? 3 l -> reset 80 column font.
    ///
    /// Additionally,
    ///
    /// * set margins to default positions
    /// * erases all data in page memory
    /// * resets DECLRMM to unavailable
    /// * clears data from the status line (if set to host-writable)
    ColumnMode = 3,
    /// IRM Insert Mode.
    ///
    /// NB should be part of non-private mode enum.
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
    /// ?80
    SixelScrolling = 80,
    /// ?1000
    ReportMouseClicks = 1000,
    /// ?1002
    ReportCellMouseMotion = 1002,
    /// ?1003
    ReportAllMouseMotion = 1003,
    /// ?1004
    ReportFocusInOut = 1004,
    /// ?1005
    Utf8Mouse = 1005,
    /// ?1006
    SgrMouse = 1006,
    /// ?1007
    AlternateScroll = 1007,
    /// ?1042
    UrgencyHints = 1042,
    /// ?1049
    SwapScreenAndSetRestoreCursor = 1049,
    /// Use a private palette for each new graphic.
    SixelPrivateColorRegisters = 1070,
    /// ?2004
    BracketedPaste = 2004,
}

impl Mode {
    /// Create mode from a primitive.
    pub fn from_primitive(intermediate: Option<&u8>, num: u16) -> Option<Mode> {
        let private = match intermediate {
            Some(b'?') => true,
            None => false,
            _ => return None,
        };

        if private {
            Some(match num {
                1 => Mode::CursorKeys,
                3 => Mode::ColumnMode,
                6 => Mode::Origin,
                7 => Mode::LineWrap,
                12 => Mode::BlinkingCursor,
                25 => Mode::ShowCursor,
                80 => Mode::SixelScrolling,
                1000 => Mode::ReportMouseClicks,
                1002 => Mode::ReportCellMouseMotion,
                1003 => Mode::ReportAllMouseMotion,
                1004 => Mode::ReportFocusInOut,
                1005 => Mode::Utf8Mouse,
                1006 => Mode::SgrMouse,
                1007 => Mode::AlternateScroll,
                1042 => Mode::UrgencyHints,
                1049 => Mode::SwapScreenAndSetRestoreCursor,
                1070 => Mode::SixelPrivateColorRegisters,
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

/// Mode for clearing line.
///
/// Relative to cursor.
#[derive(Debug)]
pub enum LineClearMode {
    /// Clear right of cursor.
    Right,
    /// Clear left of cursor.
    Left,
    /// Clear entire line.
    All,
}

/// Mode for clearing terminal.
///
/// Relative to cursor.
#[derive(Debug)]
pub enum ClearMode {
    /// Clear below cursor.
    Below,
    /// Clear above cursor.
    Above,
    /// Clear entire terminal.
    All,
    /// Clear 'saved' lines (scrollback).
    Saved,
}

/// Mode for clearing tab stops.
#[derive(Debug)]
pub enum TabulationClearMode {
    /// Clear stop under cursor.
    Current,
    /// Clear all stops.
    All,
}

/// Standard colors.
///
/// The order here matters since the enum should be castable to a `usize` for
/// indexing a color list.
#[derive(Serialize, Deserialize, Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord)]
pub enum NamedColor {
    /// Black.
    Black = 0,
    /// Red.
    Red,
    /// Green.
    Green,
    /// Yellow.
    Yellow,
    /// Blue.
    Blue,
    /// Magenta.
    Magenta,
    /// Cyan.
    Cyan,
    /// White.
    White,
    /// Bright black.
    BrightBlack,
    /// Bright red.
    BrightRed,
    /// Bright green.
    BrightGreen,
    /// Bright yellow.
    BrightYellow,
    /// Bright blue.
    BrightBlue,
    /// Bright magenta.
    BrightMagenta,
    /// Bright cyan.
    BrightCyan,
    /// Bright white.
    BrightWhite,
    /// The foreground color.
    Foreground = 256,
    /// The background color.
    Background,
    /// Color for the cursor itself.
    Cursor,
    /// Dim black.
    DimBlack,
    /// Dim red.
    DimRed,
    /// Dim green.
    DimGreen,
    /// Dim yellow.
    DimYellow,
    /// Dim blue.
    DimBlue,
    /// Dim magenta.
    DimMagenta,
    /// Dim cyan.
    DimCyan,
    /// Dim white.
    DimWhite,
    /// The bright foreground color.
    BrightForeground,
    /// Dim foreground.
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Named(NamedColor),
    Spec(Rgb),
    Indexed(u8),
}

/// Terminal character attributes.
#[derive(Debug, Eq, PartialEq)]
pub enum Attr {
    /// Clear all special abilities.
    Reset,
    /// Bold text.
    Bold,
    /// Dim or secondary color.
    Dim,
    /// Italic text.
    Italic,
    /// Underline text.
    Underline,
    /// Underlined twice.
    DoubleUnderline,
    /// Blink cursor slowly.
    BlinkSlow,
    /// Blink cursor fast.
    BlinkFast,
    /// Invert colors.
    Reverse,
    /// Do not display characters.
    Hidden,
    /// Strikeout text.
    Strike,
    /// Cancel bold.
    CancelBold,
    /// Cancel bold and dim.
    CancelBoldDim,
    /// Cancel italic.
    CancelItalic,
    /// Cancel all underlines.
    CancelUnderline,
    /// Cancel blink.
    CancelBlink,
    /// Cancel inversion.
    CancelReverse,
    /// Cancel text hiding.
    CancelHidden,
    /// Cancel strikeout.
    CancelStrike,
    /// Set indexed foreground color.
    Foreground(Color),
    /// Set indexed background color.
    Background(Color),
}

/// Identifiers which can be assigned to a graphic character set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CharsetIndex {
    /// Default set, is designated as ASCII at startup.
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

/// Standard or common character sets which can be designated as G0-G3.
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

impl StandardCharset {
    /// Switch/Map character to the active charset. Ascii is the common case and
    /// for that we want to do as little as possible.
    #[inline]
    pub fn map(self, c: char) -> char {
        match self {
            StandardCharset::Ascii => c,
            StandardCharset::SpecialCharacterAndLineDrawing => match c {
                '`' => '◆',
                'a' => '▒',
                'b' => '\t',
                'c' => '\u{000c}',
                'd' => '\r',
                'e' => '\n',
                'f' => '°',
                'g' => '±',
                'h' => '\u{2424}',
                'i' => '\u{000b}',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                _ => c,
            },
        }
    }
}

impl<'a, H> vte::Perform for Performer<'a, H>
where
    H: Handler + 'a,
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
            _ => debug!("[unhandled] execute byte={:02x}", byte),
        }
    }

    #[inline]
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        match (action, intermediates) {
            ('s', [b'=']) => {
                // Start a synchronized update. The end is handled with a separate parser.
                if params.iter().next().map_or(false, |param| param[0] == 1) {
                    self.state.dcs = Some(Dcs::SyncStart);
                }
            },
            ('q', []) => {
                let parser = self.handler.start_sixel_graphic(params);
                self.state.dcs = parser.map(Dcs::SixelData);
            },
            _ => debug!(
                "[unhandled hook] params={:?}, ints: {:?}, ignore: {:?}, action: {:?}",
                params, intermediates, ignore, action
            ),
        }
    }

    #[inline]
    fn put(&mut self, byte: u8) {
        match self.state.dcs {
            Some(Dcs::SixelData(ref mut parser)) => {
                if let Err(err) = parser.put(byte) {
                    log::warn!("Failed to parse Sixel data: {}", err);
                    self.state.dcs = None;
                }
            },

            _ => debug!("[unhandled put] byte={:?}", byte),
        }
    }

    #[inline]
    fn unhook(&mut self) {
        match self.state.dcs.take() {
            Some(Dcs::SyncStart) => {
                self.state.sync_state.timeout = Some(Instant::now() + SYNC_UPDATE_TIMEOUT);
            },
            Some(Dcs::SyncEnd) => (),
            Some(Dcs::SixelData(parser)) => match parser.finish() {
                Ok((graphic, palette)) => self.handler.insert_graphic(graphic, Some(palette)),
                Err(err) => log::warn!("Failed to parse Sixel data: {}", err),
            },
            _ => debug!("[unhandled unhook]"),
        }
    }

    #[inline]
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };

        fn unhandled(params: &[&[u8]]) {
            let mut buf = String::new();
            for items in params {
                buf.push('[');
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
            // Set window title.
            b"0" | b"2" => {
                if params.len() >= 2 {
                    let title = params[1..]
                        .iter()
                        .flat_map(|x| str::from_utf8(x))
                        .collect::<Vec<&str>>()
                        .join(";")
                        .trim()
                        .to_owned();
                    self.handler.set_title(Some(title));
                    return;
                }
                unhandled(params);
            },

            // Set color index.
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

            // Get/set Foreground, Background, Cursor colors.
            b"10" | b"11" | b"12" => {
                if params.len() >= 2 {
                    if let Some(mut dynamic_code) = parse_number(params[0]) {
                        for param in &params[1..] {
                            // 10 is the first dynamic color, also the foreground.
                            let offset = dynamic_code as usize - 10;
                            let index = NamedColor::Foreground as usize + offset;

                            // End of setting dynamic colors.
                            if index > NamedColor::Cursor as usize {
                                unhandled(params);
                                break;
                            }

                            if let Some(color) = xparse_color(param) {
                                self.handler.set_color(index, color);
                            } else if param == b"?" {
                                self.handler.dynamic_color_sequence(
                                    dynamic_code,
                                    index,
                                    terminator,
                                );
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

            // Set cursor style.
            b"50" => {
                if params.len() >= 2
                    && params[1].len() >= 13
                    && params[1][0..12] == *b"CursorShape="
                {
                    let shape = match params[1][12] as char {
                        '0' => CursorShape::Block,
                        '1' => CursorShape::Beam,
                        '2' => CursorShape::Underline,
                        _ => return unhandled(params),
                    };
                    self.handler.set_cursor_shape(shape);
                    return;
                }
                unhandled(params);
            },

            // Set clipboard.
            b"52" => {
                if params.len() < 3 {
                    return unhandled(params);
                }

                let clipboard = params[1].get(0).unwrap_or(&b'c');
                match params[2] {
                    b"?" => self.handler.clipboard_load(*clipboard, terminator),
                    base64 => self.handler.clipboard_store(*clipboard, base64),
                }
            },

            // Reset color index.
            b"104" => {
                // Reset all color indexes when no parameters are given.
                if params.len() == 1 {
                    for i in 0..256 {
                        self.handler.reset_color(i);
                    }
                    return;
                }

                // Reset color indexes given as parameters.
                for param in &params[1..] {
                    match parse_number(param) {
                        Some(index) => self.handler.reset_color(index as usize),
                        None => unhandled(params),
                    }
                }
            },

            // Reset foreground color.
            b"110" => self.handler.reset_color(NamedColor::Foreground as usize),

            // Reset background color.
            b"111" => self.handler.reset_color(NamedColor::Background as usize),

            // Reset text cursor color.
            b"112" => self.handler.reset_color(NamedColor::Cursor as usize),

            _ => unhandled(params),
        }
    }

    #[allow(clippy::cognitive_complexity)]
    #[inline]
    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        has_ignored_intermediates: bool,
        action: char,
    ) {
        macro_rules! unhandled {
            () => {{
                debug!(
                    "[Unhandled CSI] action={:?}, params={:?}, intermediates={:?}",
                    action, params, intermediates
                );
            }};
        }

        if has_ignored_intermediates || intermediates.len() > 1 {
            unhandled!();
            return;
        }

        let mut params_iter = params.iter();
        let handler = &mut self.handler;

        let mut next_param_or = |default: u16| {
            params_iter.next().map(|param| param[0]).filter(|&param| param != 0).unwrap_or(default)
        };

        match (action, intermediates) {
            ('@', []) => handler.insert_blank(next_param_or(1) as usize),
            ('A', []) => handler.move_up(next_param_or(1) as usize),
            ('B', []) | ('e', []) => handler.move_down(next_param_or(1) as usize),
            ('b', []) => {
                if let Some(c) = self.state.preceding_char {
                    for _ in 0..next_param_or(1) {
                        handler.input(c);
                    }
                } else {
                    debug!("tried to repeat with no preceding char");
                }
            },
            ('C', []) | ('a', []) => handler.move_forward(Column(next_param_or(1) as usize)),
            ('c', intermediates) if next_param_or(0) == 0 => {
                handler.identify_terminal(intermediates.get(0).map(|&i| i as char))
            },
            ('D', []) => handler.move_backward(Column(next_param_or(1) as usize)),
            ('d', []) => handler.goto_line(Line(next_param_or(1) as i32 - 1)),
            ('E', []) => handler.move_down_and_cr(next_param_or(1) as usize),
            ('F', []) => handler.move_up_and_cr(next_param_or(1) as usize),
            ('G', []) | ('`', []) => handler.goto_col(Column(next_param_or(1) as usize - 1)),
            ('g', []) => {
                let mode = match next_param_or(0) {
                    0 => TabulationClearMode::Current,
                    3 => TabulationClearMode::All,
                    _ => {
                        unhandled!();
                        return;
                    },
                };

                handler.clear_tabs(mode);
            },
            ('H', []) | ('f', []) => {
                let y = next_param_or(1) as i32;
                let x = next_param_or(1) as usize;
                handler.goto(Line(y - 1), Column(x - 1));
            },
            ('h', intermediates) => {
                for param in params_iter.map(|param| param[0]) {
                    match Mode::from_primitive(intermediates.get(0), param) {
                        Some(mode) => handler.set_mode(mode),
                        None => unhandled!(),
                    }
                }
            },
            ('I', []) => handler.move_forward_tabs(next_param_or(1)),
            ('J', []) => {
                let mode = match next_param_or(0) {
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
            ('K', []) => {
                let mode = match next_param_or(0) {
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
            ('L', []) => handler.insert_blank_lines(next_param_or(1) as usize),
            ('l', intermediates) => {
                for param in params_iter.map(|param| param[0]) {
                    match Mode::from_primitive(intermediates.get(0), param) {
                        Some(mode) => handler.unset_mode(mode),
                        None => unhandled!(),
                    }
                }
            },
            ('M', []) => handler.delete_lines(next_param_or(1) as usize),
            ('m', []) => {
                if params.is_empty() {
                    handler.terminal_attribute(Attr::Reset);
                } else {
                    for attr in attrs_from_sgr_parameters(&mut params_iter) {
                        match attr {
                            Some(attr) => handler.terminal_attribute(attr),
                            None => unhandled!(),
                        }
                    }
                }
            },
            ('n', []) => handler.device_status(next_param_or(0) as usize),
            ('P', []) => handler.delete_chars(next_param_or(1) as usize),
            ('q', [b' ']) => {
                // DECSCUSR (CSI Ps SP q) -- Set Cursor Style.
                let cursor_style_id = next_param_or(0);
                let shape = match cursor_style_id {
                    0 => None,
                    1 | 2 => Some(CursorShape::Block),
                    3 | 4 => Some(CursorShape::Underline),
                    5 | 6 => Some(CursorShape::Beam),
                    _ => {
                        unhandled!();
                        return;
                    },
                };
                let cursor_style =
                    shape.map(|shape| CursorStyle { shape, blinking: cursor_style_id % 2 == 1 });

                handler.set_cursor_style(cursor_style);
            },
            ('r', []) => {
                let top = next_param_or(1) as usize;
                let bottom =
                    params_iter.next().map(|param| param[0] as usize).filter(|&param| param != 0);

                handler.set_scrolling_region(top, bottom);
            },
            ('S', []) => handler.scroll_up(next_param_or(1) as usize),
            ('S', [b'?']) => handler.graphics_attribute(next_param_or(0), next_param_or(0)),
            ('s', []) => handler.save_cursor_position(),
            ('T', []) => handler.scroll_down(next_param_or(1) as usize),
            ('t', []) => match next_param_or(1) as usize {
                14 => handler.text_area_size_pixels(),
                18 => handler.text_area_size_chars(),
                22 => handler.push_title(),
                23 => handler.pop_title(),
                _ => unhandled!(),
            },
            ('u', []) => handler.restore_cursor_position(),
            ('X', []) => handler.erase_chars(Column(next_param_or(1) as usize)),
            ('Z', []) => handler.move_backward_tabs(next_param_or(1)),
            _ => unhandled!(),
        }
    }

    #[inline]
    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        macro_rules! unhandled {
            () => {{
                debug!(
                    "[unhandled] esc_dispatch ints={:?}, byte={:?} ({:02x})",
                    intermediates, byte as char, byte
                );
            }};
        }

        macro_rules! configure_charset {
            ($charset:path, $intermediates:expr) => {{
                let index: CharsetIndex = match $intermediates {
                    [b'('] => CharsetIndex::G0,
                    [b')'] => CharsetIndex::G1,
                    [b'*'] => CharsetIndex::G2,
                    [b'+'] => CharsetIndex::G3,
                    _ => {
                        unhandled!();
                        return;
                    },
                };
                self.handler.configure_charset(index, $charset)
            }};
        }

        match (byte, intermediates) {
            (b'B', intermediates) => configure_charset!(StandardCharset::Ascii, intermediates),
            (b'D', []) => self.handler.linefeed(),
            (b'E', []) => {
                self.handler.linefeed();
                self.handler.carriage_return();
            },
            (b'H', []) => self.handler.set_horizontal_tabstop(),
            (b'M', []) => self.handler.reverse_index(),
            (b'Z', []) => self.handler.identify_terminal(None),
            (b'c', []) => self.handler.reset_state(),
            (b'0', intermediates) => {
                configure_charset!(StandardCharset::SpecialCharacterAndLineDrawing, intermediates)
            },
            (b'7', []) => self.handler.save_cursor_position(),
            (b'8', [b'#']) => self.handler.decaln(),
            (b'8', []) => self.handler.restore_cursor_position(),
            (b'=', []) => self.handler.set_keypad_application_mode(),
            (b'>', []) => self.handler.unset_keypad_application_mode(),
            // String terminator, do nothing (parser handles as string terminator).
            (b'\\', []) => (),
            _ => unhandled!(),
        }
    }
}

#[inline]
fn attrs_from_sgr_parameters(params: &mut ParamsIter<'_>) -> Vec<Option<Attr>> {
    let mut attrs = Vec::with_capacity(params.size_hint().0);

    while let Some(param) = params.next() {
        let attr = match param {
            [0] => Some(Attr::Reset),
            [1] => Some(Attr::Bold),
            [2] => Some(Attr::Dim),
            [3] => Some(Attr::Italic),
            [4, 0] => Some(Attr::CancelUnderline),
            [4, 2] => Some(Attr::DoubleUnderline),
            [4, ..] => Some(Attr::Underline),
            [5] => Some(Attr::BlinkSlow),
            [6] => Some(Attr::BlinkFast),
            [7] => Some(Attr::Reverse),
            [8] => Some(Attr::Hidden),
            [9] => Some(Attr::Strike),
            [21] => Some(Attr::CancelBold),
            [22] => Some(Attr::CancelBoldDim),
            [23] => Some(Attr::CancelItalic),
            [24] => Some(Attr::CancelUnderline),
            [25] => Some(Attr::CancelBlink),
            [27] => Some(Attr::CancelReverse),
            [28] => Some(Attr::CancelHidden),
            [29] => Some(Attr::CancelStrike),
            [30] => Some(Attr::Foreground(Color::Named(NamedColor::Black))),
            [31] => Some(Attr::Foreground(Color::Named(NamedColor::Red))),
            [32] => Some(Attr::Foreground(Color::Named(NamedColor::Green))),
            [33] => Some(Attr::Foreground(Color::Named(NamedColor::Yellow))),
            [34] => Some(Attr::Foreground(Color::Named(NamedColor::Blue))),
            [35] => Some(Attr::Foreground(Color::Named(NamedColor::Magenta))),
            [36] => Some(Attr::Foreground(Color::Named(NamedColor::Cyan))),
            [37] => Some(Attr::Foreground(Color::Named(NamedColor::White))),
            [38] => {
                let mut iter = params.map(|param| param[0]);
                parse_sgr_color(&mut iter).map(Attr::Foreground)
            },
            [38, params @ ..] => {
                let rgb_start = if params.len() > 4 { 2 } else { 1 };
                let rgb_iter = params[rgb_start..].iter().copied();
                let mut iter = iter::once(params[0]).chain(rgb_iter);

                parse_sgr_color(&mut iter).map(Attr::Foreground)
            },
            [39] => Some(Attr::Foreground(Color::Named(NamedColor::Foreground))),
            [40] => Some(Attr::Background(Color::Named(NamedColor::Black))),
            [41] => Some(Attr::Background(Color::Named(NamedColor::Red))),
            [42] => Some(Attr::Background(Color::Named(NamedColor::Green))),
            [43] => Some(Attr::Background(Color::Named(NamedColor::Yellow))),
            [44] => Some(Attr::Background(Color::Named(NamedColor::Blue))),
            [45] => Some(Attr::Background(Color::Named(NamedColor::Magenta))),
            [46] => Some(Attr::Background(Color::Named(NamedColor::Cyan))),
            [47] => Some(Attr::Background(Color::Named(NamedColor::White))),
            [48] => {
                let mut iter = params.map(|param| param[0]);
                parse_sgr_color(&mut iter).map(Attr::Background)
            },
            [48, params @ ..] => {
                let rgb_start = if params.len() > 4 { 2 } else { 1 };
                let rgb_iter = params[rgb_start..].iter().copied();
                let mut iter = iter::once(params[0]).chain(rgb_iter);

                parse_sgr_color(&mut iter).map(Attr::Background)
            },
            [49] => Some(Attr::Background(Color::Named(NamedColor::Background))),
            [90] => Some(Attr::Foreground(Color::Named(NamedColor::BrightBlack))),
            [91] => Some(Attr::Foreground(Color::Named(NamedColor::BrightRed))),
            [92] => Some(Attr::Foreground(Color::Named(NamedColor::BrightGreen))),
            [93] => Some(Attr::Foreground(Color::Named(NamedColor::BrightYellow))),
            [94] => Some(Attr::Foreground(Color::Named(NamedColor::BrightBlue))),
            [95] => Some(Attr::Foreground(Color::Named(NamedColor::BrightMagenta))),
            [96] => Some(Attr::Foreground(Color::Named(NamedColor::BrightCyan))),
            [97] => Some(Attr::Foreground(Color::Named(NamedColor::BrightWhite))),
            [100] => Some(Attr::Background(Color::Named(NamedColor::BrightBlack))),
            [101] => Some(Attr::Background(Color::Named(NamedColor::BrightRed))),
            [102] => Some(Attr::Background(Color::Named(NamedColor::BrightGreen))),
            [103] => Some(Attr::Background(Color::Named(NamedColor::BrightYellow))),
            [104] => Some(Attr::Background(Color::Named(NamedColor::BrightBlue))),
            [105] => Some(Attr::Background(Color::Named(NamedColor::BrightMagenta))),
            [106] => Some(Attr::Background(Color::Named(NamedColor::BrightCyan))),
            [107] => Some(Attr::Background(Color::Named(NamedColor::BrightWhite))),
            _ => None,
        };
        attrs.push(attr);
    }

    attrs
}

/// Parse a color specifier from list of attributes.
fn parse_sgr_color(params: &mut dyn Iterator<Item = u16>) -> Option<Color> {
    match params.next() {
        Some(2) => Some(Color::Spec(Rgb {
            r: u8::try_from(params.next()?).ok()?,
            g: u8::try_from(params.next()?).ok()?,
            b: u8::try_from(params.next()?).ok()?,
        })),
        Some(5) => Some(Color::Indexed(u8::try_from(params.next()?).ok()?)),
        _ => None,
    }
}

/// C0 set of 7-bit control characters (from ANSI X3.4-1977).
#[allow(non_snake_case)]
pub mod C0 {
    /// Null filler, terminal should ignore this character.
    pub const NUL: u8 = 0x00;
    /// Start of Header.
    pub const SOH: u8 = 0x01;
    /// Start of Text, implied end of header.
    pub const STX: u8 = 0x02;
    /// End of Text, causes some terminal to respond with ACK or NAK.
    pub const ETX: u8 = 0x03;
    /// End of Transmission.
    pub const EOT: u8 = 0x04;
    /// Enquiry, causes terminal to send ANSWER-BACK ID.
    pub const ENQ: u8 = 0x05;
    /// Acknowledge, usually sent by terminal in response to ETX.
    pub const ACK: u8 = 0x06;
    /// Bell, triggers the bell, buzzer, or beeper on the terminal.
    pub const BEL: u8 = 0x07;
    /// Backspace, can be used to define overstruck characters.
    pub const BS: u8 = 0x08;
    /// Horizontal Tabulation, move to next predetermined position.
    pub const HT: u8 = 0x09;
    /// Linefeed, move to same position on next line (see also NL).
    pub const LF: u8 = 0x0A;
    /// Vertical Tabulation, move to next predetermined line.
    pub const VT: u8 = 0x0B;
    /// Form Feed, move to next form or page.
    pub const FF: u8 = 0x0C;
    /// Carriage Return, move to first character of current line.
    pub const CR: u8 = 0x0D;
    /// Shift Out, switch to G1 (other half of character set).
    pub const SO: u8 = 0x0E;
    /// Shift In, switch to G0 (normal half of character set).
    pub const SI: u8 = 0x0F;
    /// Data Link Escape, interpret next control character specially.
    pub const DLE: u8 = 0x10;
    /// (DC1) Terminal is allowed to resume transmitting.
    pub const XON: u8 = 0x11;
    /// Device Control 2, causes ASR-33 to activate paper-tape reader.
    pub const DC2: u8 = 0x12;
    /// (DC2) Terminal must pause and refrain from transmitting.
    pub const XOFF: u8 = 0x13;
    /// Device Control 4, causes ASR-33 to deactivate paper-tape reader.
    pub const DC4: u8 = 0x14;
    /// Negative Acknowledge, used sometimes with ETX and ACK.
    pub const NAK: u8 = 0x15;
    /// Synchronous Idle, used to maintain timing in Sync communication.
    pub const SYN: u8 = 0x16;
    /// End of Transmission block.
    pub const ETB: u8 = 0x17;
    /// Cancel (makes VT100 abort current escape sequence if any).
    pub const CAN: u8 = 0x18;
    /// End of Medium.
    pub const EM: u8 = 0x19;
    /// Substitute (VT100 uses this to display parity errors).
    pub const SUB: u8 = 0x1A;
    /// Prefix to an escape sequence.
    pub const ESC: u8 = 0x1B;
    /// File Separator.
    pub const FS: u8 = 0x1C;
    /// Group Separator.
    pub const GS: u8 = 0x1D;
    /// Record Separator (sent by VT132 in block-transfer mode).
    pub const RS: u8 = 0x1E;
    /// Unit Separator.
    pub const US: u8 = 0x1F;
    /// Delete, should be ignored by terminal.
    pub const DEL: u8 = 0x7f;
}

// Tests for parsing escape sequences.
//
// Byte sequences used in these tests are recording of pty stdout.
#[cfg(test)]
mod tests {
    use super::*;

    use crate::term::color::Rgb;

    struct MockHandler {
        index: CharsetIndex,
        charset: StandardCharset,
        attr: Option<Attr>,
        identity_reported: bool,
    }

    impl Handler for MockHandler {
        fn terminal_attribute(&mut self, attr: Attr) {
            self.attr = Some(attr);
        }

        fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
            self.index = index;
            self.charset = charset;
        }

        fn set_active_charset(&mut self, index: CharsetIndex) {
            self.index = index;
        }

        fn identify_terminal(&mut self, _intermediate: Option<char>) {
            self.identity_reported = true;
        }

        fn reset_state(&mut self) {
            *self = Self::default();
        }
    }

    impl Default for MockHandler {
        fn default() -> MockHandler {
            MockHandler {
                index: CharsetIndex::G0,
                charset: StandardCharset::Ascii,
                attr: None,
                identity_reported: false,
            }
        }
    }

    #[test]
    fn parse_control_attribute() {
        static BYTES: &[u8] = &[0x1b, b'[', b'1', b'm'];

        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in BYTES {
            parser.advance(&mut handler, *byte);
        }

        assert_eq!(handler.attr, Some(Attr::Bold));
    }

    #[test]
    fn parse_terminal_identity_csi() {
        let bytes: &[u8] = &[0x1b, b'[', b'1', b'c'];

        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in bytes {
            parser.advance(&mut handler, *byte);
        }

        assert!(!handler.identity_reported);
        handler.reset_state();

        let bytes: &[u8] = &[0x1b, b'[', b'c'];

        for byte in bytes {
            parser.advance(&mut handler, *byte);
        }

        assert!(handler.identity_reported);
        handler.reset_state();

        let bytes: &[u8] = &[0x1b, b'[', b'0', b'c'];

        for byte in bytes {
            parser.advance(&mut handler, *byte);
        }

        assert!(handler.identity_reported);
    }

    #[test]
    fn parse_terminal_identity_esc() {
        let bytes: &[u8] = &[0x1b, b'Z'];

        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in bytes {
            parser.advance(&mut handler, *byte);
        }

        assert!(handler.identity_reported);
        handler.reset_state();

        let bytes: &[u8] = &[0x1b, b'#', b'Z'];

        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in bytes {
            parser.advance(&mut handler, *byte);
        }

        assert!(!handler.identity_reported);
        handler.reset_state();
    }

    #[test]
    fn parse_truecolor_attr() {
        static BYTES: &[u8] = &[
            0x1b, b'[', b'3', b'8', b';', b'2', b';', b'1', b'2', b'8', b';', b'6', b'6', b';',
            b'2', b'5', b'5', b'm',
        ];

        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in BYTES {
            parser.advance(&mut handler, *byte);
        }

        let spec = Rgb { r: 128, g: 66, b: 255 };

        assert_eq!(handler.attr, Some(Attr::Foreground(Color::Spec(spec))));
    }

    /// No exactly a test; useful for debugging.
    #[test]
    fn parse_zsh_startup() {
        static BYTES: &[u8] = &[
            0x1b, b'[', b'1', b'm', 0x1b, b'[', b'7', b'm', b'%', 0x1b, b'[', b'2', b'7', b'm',
            0x1b, b'[', b'1', b'm', 0x1b, b'[', b'0', b'm', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b'\r', b' ', b'\r', b'\r', 0x1b, b'[', b'0', b'm', 0x1b, b'[', b'2',
            b'7', b'm', 0x1b, b'[', b'2', b'4', b'm', 0x1b, b'[', b'J', b'j', b'w', b'i', b'l',
            b'm', b'@', b'j', b'w', b'i', b'l', b'm', b'-', b'd', b'e', b's', b'k', b' ', 0x1b,
            b'[', b'0', b'1', b';', b'3', b'2', b'm', 0xe2, 0x9e, 0x9c, b' ', 0x1b, b'[', b'0',
            b'1', b';', b'3', b'2', b'm', b' ', 0x1b, b'[', b'3', b'6', b'm', b'~', b'/', b'c',
            b'o', b'd', b'e',
        ];

        let mut handler = MockHandler::default();
        let mut parser = Processor::new();

        for byte in BYTES {
            parser.advance(&mut handler, *byte);
        }
    }

    #[test]
    fn parse_designate_g0_as_line_drawing() {
        static BYTES: &[u8] = &[0x1b, b'(', b'0'];
        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in BYTES {
            parser.advance(&mut handler, *byte);
        }

        assert_eq!(handler.index, CharsetIndex::G0);
        assert_eq!(handler.charset, StandardCharset::SpecialCharacterAndLineDrawing);
    }

    #[test]
    fn parse_designate_g1_as_line_drawing_and_invoke() {
        static BYTES: &[u8] = &[0x1b, b')', b'0', 0x0e];
        let mut parser = Processor::new();
        let mut handler = MockHandler::default();

        for byte in &BYTES[..3] {
            parser.advance(&mut handler, *byte);
        }

        assert_eq!(handler.index, CharsetIndex::G1);
        assert_eq!(handler.charset, StandardCharset::SpecialCharacterAndLineDrawing);

        let mut handler = MockHandler::default();
        parser.advance(&mut handler, BYTES[3]);

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
