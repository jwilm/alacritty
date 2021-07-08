//! This module implements a parser for the Sixel protocol, and it is based on the
//! chapter [SIXEL GRAPHICS EXTENSION] of the DEC reference manual.
//!
//! [SIXEL GRAPHICS EXTENSION]: https://archive.org/details/bitsavers_decstandar0VideoSystemsReferenceManualDec91_74264381/page/n907/mode/2up
//!
//! # Limitations
//!
//! The parser have the following limitations:
//!
//! * A single image can use up to 1024 different colors.
//!
//!   The Sixel reference requires 256, but allow more colors.
//!
//! * Image dimensions are limited to 4096 x 4096.
//!
//! * Pixel aspect ratio parameters are ignored.
//!
//!   The Sixel references specifies some parameters to change the pixel
//!   aspect ratio, but multiple implementations always use 1:1, so these
//!   parameters have no real effect.
use std::cmp::max;
use std::fmt;
use std::mem;

use crate::graphics::{ColorType, GraphicData, GraphicId, MAX_GRAPHIC_DIMENSIONS};
use crate::term::color::Rgb;

use log::trace;
use vte::Params;

/// Type for color registers.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
struct ColorRegister(u16);

/// Number of color registers.
pub const MAX_COLOR_REGISTERS: usize = 1024;

/// Color register for transparent pixels.
const REG_TRANSPARENT: ColorRegister = ColorRegister(u16::MAX);

/// Number of parameters allowed in a single Sixel command.
const MAX_COMMAND_PARAMS: usize = 5;

#[derive(Debug)]
pub enum Error {
    /// Image dimensions are too big.
    TooBigImage { width: usize, height: usize },

    /// A component in a color introducer is not valid.
    InvalidColorComponent { register: u16, component_value: u16 },

    /// The coordinate system to define the color register is not valid.
    InvalidColorCoordinateSystem { register: u16, coordinate_system: u16 },
}

impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooBigImage { width, height } => {
                write!(fmt, "The image dimensions are too big ({}, {})", width, height)
            },

            Error::InvalidColorComponent { register, component_value } => {
                write!(fmt, "Invalid color component {} for register {}", component_value, register)
            },

            Error::InvalidColorCoordinateSystem { register, coordinate_system } => {
                write!(
                    fmt,
                    "Invalid color coordinate system {} for register {}",
                    coordinate_system, register
                )
            },
        }
    }
}

/// Commands found in the data stream.
#[derive(Debug)]
enum SixelCommand {
    /// Specifies a repeat count before a sixel.
    ///
    /// Its only parameter is the repeat count.
    RepeatIntroducer,

    /// Defines raster attributes for the following data.
    ///
    /// It expects 4 parameters:
    ///
    /// 1. Pixel aspect ratio numerator (relative height).
    /// 2. Pixel aspect ratio denominator (relative width).
    /// 3. Horizontal Extent.
    /// 4. Vertical Extent.
    SetRasterAttributes,

    /// Starts a color selection sequence.
    ///
    /// The first parameter is the register number.
    ///
    /// Optionally, it can receive 4 more parameters:
    ///
    /// 1. Color coordinate system. `1` for HLS, `2` for RGB.
    /// 2. Hue angle, or red.
    /// 3. Lightness, or green.
    /// 4. Saturation, or blue.
    ColorIntroducer,

    /// Moves the active position to the graphic left margin.
    CarriageReturn,

    /// Moves the active position to the graphic left margin
    /// and one row of sixels.
    NextLine,
}

/// Parser for commands found in the picture definition.
#[derive(Debug)]
struct CommandParser {
    /// Active command.
    command: SixelCommand,

    /// Parameter values.
    ///
    /// If a value is greater than `u16::MAX`, it will be kept as `u16::MAX`.
    ///
    /// Parameters after `MAX_COMMAND_PARAMS` are ignored.
    params: [u16; MAX_COMMAND_PARAMS],

    /// Current position.
    params_position: usize,
}

impl CommandParser {
    fn new(command: SixelCommand) -> CommandParser {
        CommandParser { command, params: [0; MAX_COMMAND_PARAMS], params_position: 0 }
    }

    fn put(&mut self, byte: u8) {
        let pos = self.params_position;
        if pos < MAX_COMMAND_PARAMS {
            match byte {
                b'0'..=b'9' => {
                    self.params[pos] =
                        self.params[pos].saturating_mul(10).saturating_add((byte - b'0') as u16);
                },

                b';' => {
                    self.params_position += 1;
                },

                _ => (), // Ignore unknown bytes.
            }
        }
    }

    /// Apply the execution of the active command to the parser.
    fn finish(self, parser: &mut Parser) -> Result<(), Error> {
        match self.command {
            SixelCommand::RepeatIntroducer => {
                parser.repeat_count = self.params[0] as usize;
            },

            SixelCommand::SetRasterAttributes => {
                if self.params_position >= 3 {
                    let width = self.params[2] as usize;
                    let height = self.params[3] as usize;
                    parser.ensure_size(width, height)?;
                }
            },

            SixelCommand::ColorIntroducer => {
                let register = ColorRegister(self.params[0]);

                if self.params_position >= 4 {
                    macro_rules! p {
                        ($index:expr) => {
                            match self.params[$index] {
                                x if x <= 100 => x,
                                x => {
                                    return Err(Error::InvalidColorComponent {
                                        register: register.0,
                                        component_value: x,
                                    })
                                },
                            }
                        };
                    }

                    let (r, g, b) = match self.params[1] {
                        // HLS.
                        1 => hls_to_rgb(p!(2), p!(3), p!(4)),

                        // RGB.
                        2 => (p!(2), p!(3), p!(4)),

                        // Invalid coordinate system.
                        x => {
                            return Err(Error::InvalidColorCoordinateSystem {
                                register: register.0,
                                coordinate_system: x,
                            })
                        },
                    };

                    parser.set_color_register(register, r, g, b);
                }

                parser.selected_color_register = register;
            },

            SixelCommand::CarriageReturn => {
                parser.x = 0;
            },

            SixelCommand::NextLine => {
                parser.x = 0;
                parser.y += 6;
            },
        }

        Ok(())
    }
}

/// A group of 6 vertical pixels.
struct Sixel(u8);

impl Sixel {
    /// Create a new sixel.
    ///
    /// It expects the byte value from the picture definition stream.
    #[inline]
    fn new(byte: u8) -> Sixel {
        debug_assert!((0x3F..=0x7E).contains(&byte));
        Sixel(byte - 0x3F)
    }

    /// Return how many rows are printed in the sixel.
    #[inline]
    fn height(&self) -> usize {
        8 - self.0.leading_zeros() as usize
    }

    /// Return an iterator to get dots in the sixel.
    #[inline]
    fn dots(&self) -> impl Iterator<Item = bool> {
        let sixel = self.0;
        (0..6).map(move |position| sixel & (1 << position) != 0)
    }
}

/// Parser of the picture definition in a Sixel data stream.
#[derive(Default, Debug)]
pub struct Parser {
    /// Active command to be parsed.
    command_parser: Option<CommandParser>,

    /// Current picture width.
    width: usize,

    /// Current picture height.
    height: usize,

    /// Current picture pixels.
    pixels: Vec<ColorRegister>,

    /// Indicates the register color for empty pixels.
    background: ColorRegister,

    /// RGB values for every register.
    color_registers: Vec<Rgb>,

    /// Selected color register.
    selected_color_register: ColorRegister,

    /// Repeat count for the next sixel.
    repeat_count: usize,

    /// Horizontal position of the active sixel.
    x: usize,

    /// Vertical position of the active sixel.
    y: usize,
}

impl Parser {
    /// Creates a new parser.
    pub fn new(params: &Params, shared_palette: Option<Vec<Rgb>>) -> Parser {
        trace!("Start Sixel parser");

        let mut parser = Parser::default();

        // According to the Sixel reference, the second parameter (Ps2) is
        // the background selector. It controls how to show pixels without
        // an explicit color, and it accepts the following values:
        //
        //   0   device default action
        //   1   no action (don't change zero value pixels)
        //   2   set zero value pixels to background color
        //
        // We replicate the xterm's behaviour:
        //
        //  - If it is set to `1`, the background is transparent.
        //  - For any other value, the background is the color register 0.

        let ps2 = params.iter().nth(1).and_then(|param| param.iter().next().copied()).unwrap_or(0);
        parser.background = if ps2 == 1 { REG_TRANSPARENT } else { ColorRegister(0) };

        if let Some(color_registers) = shared_palette {
            parser.color_registers = color_registers;
        } else {
            init_color_registers(&mut parser);
        }

        parser
    }

    /// Parse a byte from the Sixel stream.
    pub fn put(&mut self, byte: u8) -> Result<(), Error> {
        match byte {
            b'!' => self.start_command(SixelCommand::RepeatIntroducer)?,

            b'"' => self.start_command(SixelCommand::SetRasterAttributes)?,

            b'#' => self.start_command(SixelCommand::ColorIntroducer)?,

            b'$' => self.start_command(SixelCommand::CarriageReturn)?,

            b'-' => self.start_command(SixelCommand::NextLine)?,

            b'0'..=b'9' | b';' => {
                if let Some(command_parser) = &mut self.command_parser {
                    command_parser.put(byte);
                }
            },

            0x3F..=0x7E => self.add_sixel(Sixel::new(byte))?,

            _ => {
                // Invalid bytes are ignored, but we still have to finish any
                // active command.

                self.finish_command()?;
            },
        }

        Ok(())
    }

    #[inline]
    fn start_command(&mut self, command: SixelCommand) -> Result<(), Error> {
        self.finish_command()?;
        self.command_parser = Some(CommandParser::new(command));
        Ok(())
    }

    #[inline]
    fn finish_command(&mut self) -> Result<(), Error> {
        if let Some(command_parser) = self.command_parser.take() {
            command_parser.finish(self)?;
        }

        Ok(())
    }

    /// Set the RGB color for a register.
    ///
    /// Color components are expected to be in the range of 0..=100.
    fn set_color_register(&mut self, register: ColorRegister, r: u16, g: u16, b: u16) {
        let register = register.0 as usize;

        if register >= MAX_COLOR_REGISTERS {
            return;
        }

        if self.color_registers.len() <= register {
            self.color_registers.resize(register + 1, Rgb { r: 0, g: 0, b: 0 })
        }

        let r = ((r * 255 + 50) / 100) as u8;
        let g = ((g * 255 + 50) / 100) as u8;
        let b = ((b * 255 + 50) / 100) as u8;
        self.color_registers[register] = Rgb { r, g, b };
    }

    /// Check if the current picture is big enough for the given dimensions. If
    /// not, the picture is resized.
    fn ensure_size(&mut self, width: usize, height: usize) -> Result<(), Error> {
        // Do nothing if the current picture is big enough.
        if self.width >= width && self.height >= height {
            return Ok(());
        }

        if width > MAX_GRAPHIC_DIMENSIONS[0] || height > MAX_GRAPHIC_DIMENSIONS[1] {
            return Err(Error::TooBigImage { width, height });
        }

        trace!(
            "Set Sixel image dimensions to {}x{}",
            max(self.width, width),
            max(self.height, height),
        );

        // If there is no current picture, creates a new one.
        if self.pixels.is_empty() {
            self.width = width;
            self.height = height;
            self.pixels = vec![self.background; width * height];
            return Ok(());
        }

        // If current width is big enough, we only need to add more pixels
        // after the current buffer.
        if self.width >= width {
            self.pixels.resize(height * self.width, self.background);
            self.height = height;
            return Ok(());
        }

        // At this point, we know that the new width is greater than the
        // current one, so we have to extend the buffer and move the rows to
        // their new positions.
        let height = usize::max(height, self.height);

        self.pixels.resize(height * width, self.background);

        for y in (0..self.height).rev() {
            for x in (0..self.width).rev() {
                let old = y * self.width + x;
                let new = y * width + x;
                self.pixels.swap(old, new);
            }
        }

        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Add a sixel using the selected color register, and move the active
    /// position.
    fn add_sixel(&mut self, sixel: Sixel) -> Result<(), Error> {
        self.finish_command()?;

        // Take the repeat count and reset it.
        //
        // `max` function is used because the Sixel reference specifies
        // that a repeat count of zero implies a repeat count of 1.
        let repeat = max(1, mem::take(&mut self.repeat_count));

        self.ensure_size(self.x + repeat, self.y + sixel.height())?;

        if sixel.0 != 0 {
            let mut index = self.width * self.y + self.x;
            for dot in sixel.dots() {
                if dot {
                    for pixel in &mut self.pixels[index..index + repeat] {
                        *pixel = self.selected_color_register;
                    }
                }

                index += self.width;
            }
        }

        self.x += repeat;

        Ok(())
    }

    /// Returns the final graphic to append to the grid, with the palette
    /// built in the process.
    pub fn finish(mut self) -> Result<(GraphicData, Vec<Rgb>), Error> {
        self.finish_command()?;

        trace!(
            "Finish Sixel parser: width={}, height={}, color_registers={}",
            self.width,
            self.height,
            self.color_registers.len()
        );

        let mut rgba_pixels = Vec::with_capacity(self.pixels.len() * 4);

        for &register in &self.pixels {
            let pixel = {
                if register == REG_TRANSPARENT {
                    [0; 4]
                } else {
                    match self.color_registers.get(register.0 as usize) {
                        None => [0, 0, 0, 255],
                        Some(color) => [color.r, color.g, color.b, 255],
                    }
                }
            };

            rgba_pixels.extend_from_slice(&pixel);
        }

        let data = GraphicData {
            id: GraphicId(0),
            height: self.height,
            width: self.width,
            color_type: ColorType::Rgba,
            pixels: rgba_pixels,
        };

        Ok((data, self.color_registers))
    }
}

/// Compute a RGB value from HLS.
///
/// Input and output values are in the range of `0..=100`.
///
/// The implementation is a direct port of the same function in the
/// xterm's code.
#[allow(clippy::many_single_char_names)]
fn hls_to_rgb(h: u16, l: u16, s: u16) -> (u16, u16, u16) {
    if s == 0 {
        return (l, l, l);
    }

    let hs = ((h + 240) / 60) % 6;
    let lv = l as f64 / 100.0;

    let c2 = f64::abs((2.0 * lv as f64) - 1.0);
    let c = (1.0 - c2) * (s as f64 / 100.0);
    let x = if hs & 1 == 1 { c } else { 0.0 };

    let rgb = match hs {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., c),
    };

    fn clamp(x: f64) -> u16 {
        let x = x * 100. + 0.5;
        if x > 100. {
            100
        } else if x < 0. {
            0
        } else {
            x as u16
        }
    }

    let m = lv - 0.5 * c;
    let r = clamp(rgb.0 + m);
    let g = clamp(rgb.1 + m);
    let b = clamp(rgb.2 + m);

    (r, g, b)
}

/// Initialize the color registers using the colors from the VT-340 terminal.
///
/// There is no official documentation about these colors, but multiple Sixel
/// implementations assume this palette.
fn init_color_registers(parser: &mut Parser) {
    parser.set_color_register(ColorRegister(0), 0, 0, 0);
    parser.set_color_register(ColorRegister(1), 20, 20, 80);
    parser.set_color_register(ColorRegister(2), 80, 13, 13);
    parser.set_color_register(ColorRegister(3), 20, 80, 20);
    parser.set_color_register(ColorRegister(4), 80, 20, 80);
    parser.set_color_register(ColorRegister(5), 20, 80, 80);
    parser.set_color_register(ColorRegister(6), 80, 80, 20);
    parser.set_color_register(ColorRegister(7), 53, 53, 53);
    parser.set_color_register(ColorRegister(8), 26, 26, 26);
    parser.set_color_register(ColorRegister(9), 33, 33, 60);
    parser.set_color_register(ColorRegister(10), 60, 26, 26);
    parser.set_color_register(ColorRegister(11), 33, 60, 33);
    parser.set_color_register(ColorRegister(12), 60, 33, 60);
    parser.set_color_register(ColorRegister(13), 33, 60, 60);
    parser.set_color_register(ColorRegister(14), 60, 60, 33);
    parser.set_color_register(ColorRegister(15), 80, 80, 80);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    macro_rules! put_bytes {
        ($parser:expr, $data:expr) => {
            #[allow(clippy::string_lit_as_bytes)]
            for &byte in $data.as_bytes() {
                let _ = $parser.put(byte);
            }
        };
    }

    #[test]
    fn parse_command_parameters() {
        let mut command_parser = CommandParser::new(SixelCommand::ColorIntroducer);
        put_bytes!(command_parser, "65535;1;2;3;4;5");

        assert_eq!(command_parser.params_position, 5);
        assert_eq!(command_parser.params[0], 65535);
        assert_eq!(command_parser.params[1], 1);
        assert_eq!(command_parser.params[2], 2);
        assert_eq!(command_parser.params[3], 3);
        assert_eq!(command_parser.params[4], 4);
    }

    #[test]
    fn set_color_registers() {
        let mut parser = Parser::default();
        put_bytes!(parser, "#1;2;30;100;0#200;1;20;75;50.");

        assert!(parser.color_registers.len() >= 200);

        assert_eq!(parser.color_registers[1], Rgb { r: 77, g: 255, b: 0 });
        assert_eq!(parser.color_registers[200], Rgb { r: 161, g: 161, b: 224 });

        assert_eq!(parser.selected_color_register.0, 200);
    }

    #[test]
    fn convert_hls_colors() {
        // This test converts values from HLS to RBG, and compares those
        // results with the values generated by the xterm implementation
        // of the same function.

        assert_eq!(hls_to_rgb(100, 60, 60), (84, 36, 84));
        assert_eq!(hls_to_rgb(60, 100, 60), (100, 100, 100));
        assert_eq!(hls_to_rgb(30, 30, 60), (12, 12, 48));
        assert_eq!(hls_to_rgb(100, 90, 100), (100, 80, 100));
        assert_eq!(hls_to_rgb(100, 0, 90), (0, 0, 0));
        assert_eq!(hls_to_rgb(0, 90, 30), (87, 87, 93));
        assert_eq!(hls_to_rgb(60, 0, 60), (0, 0, 0));
        assert_eq!(hls_to_rgb(30, 0, 0), (0, 0, 0));
        assert_eq!(hls_to_rgb(30, 90, 30), (87, 87, 93));
        assert_eq!(hls_to_rgb(30, 30, 30), (21, 21, 39));
        assert_eq!(hls_to_rgb(90, 100, 60), (100, 100, 100));
        assert_eq!(hls_to_rgb(0, 0, 0), (0, 0, 0));
        assert_eq!(hls_to_rgb(30, 0, 90), (0, 0, 0));
        assert_eq!(hls_to_rgb(100, 60, 90), (96, 24, 96));
        assert_eq!(hls_to_rgb(30, 30, 0), (30, 30, 30));
    }

    #[test]
    fn resize_picture() -> Result<(), Error> {
        let mut parser = Parser { background: REG_TRANSPARENT, ..Parser::default() };

        const WIDTH: usize = 30;
        const HEIGHT: usize = 20;

        // Initialize a transparent picture with Set Raster Attributes.
        put_bytes!(parser, format!("\"1;1;{};{}.", WIDTH, HEIGHT));

        assert_eq!(parser.width, WIDTH);
        assert_eq!(parser.height, HEIGHT);
        assert_eq!(parser.pixels.len(), WIDTH * HEIGHT);

        assert!(parser.pixels.iter().all(|&pixel| pixel == REG_TRANSPARENT));

        // Fill each row with a different color register.
        for (n, row) in parser.pixels.chunks_mut(WIDTH).enumerate() {
            row.iter_mut().for_each(|pixel| *pixel = ColorRegister(n as u16));
        }

        // Increase height.
        //
        // New rows must be transparent.
        parser.ensure_size(WIDTH, HEIGHT + 5)?;

        assert_eq!(parser.width, WIDTH);
        assert_eq!(parser.height, HEIGHT + 5);
        assert_eq!(parser.pixels.len(), WIDTH * (HEIGHT + 5));

        for (n, row) in parser.pixels.chunks(WIDTH).enumerate() {
            let expected = if n < HEIGHT { ColorRegister(n as u16) } else { REG_TRANSPARENT };
            assert!(row.iter().all(|pixel| *pixel == expected));
        }

        // Increase both width and height.
        //
        // New rows and columns must be transparent.
        parser.ensure_size(WIDTH + 5, HEIGHT + 10)?;

        assert_eq!(parser.width, WIDTH + 5);
        assert_eq!(parser.height, HEIGHT + 10);
        assert_eq!(parser.pixels.len(), (WIDTH + 5) * (HEIGHT + 10));

        for (n, row) in parser.pixels.chunks(WIDTH + 5).enumerate() {
            if n < HEIGHT {
                assert!(row[..WIDTH].iter().all(|pixel| *pixel == ColorRegister(n as u16)));
                assert!(row[WIDTH..].iter().all(|pixel| *pixel == REG_TRANSPARENT));
            } else {
                assert!(row.iter().all(|pixel| *pixel == REG_TRANSPARENT));
            }
        }

        Ok(())
    }

    #[test]
    fn sixel_height() {
        assert_eq!(Sixel(0b000000).height(), 0);
        assert_eq!(Sixel(0b000001).height(), 1);
        assert_eq!(Sixel(0b000100).height(), 3);
        assert_eq!(Sixel(0b000101).height(), 3);
        assert_eq!(Sixel(0b101111).height(), 6);
    }

    #[test]
    fn sixel_positions() {
        macro_rules! dots {
            ($sixel:expr) => {
                Sixel($sixel).dots().collect::<Vec<_>>()
            };
        }

        assert_eq!(dots!(0b000000), &[false, false, false, false, false, false,]);
        assert_eq!(dots!(0b000001), &[true, false, false, false, false, false,]);
        assert_eq!(dots!(0b000100), &[false, false, true, false, false, false,]);
        assert_eq!(dots!(0b000101), &[true, false, true, false, false, false,]);
        assert_eq!(dots!(0b101111), &[true, true, true, true, false, true,]);
    }

    #[test]
    fn load_sixel_files() {
        let images_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/sixel"));

        let test_images = ["testimage_im6", "testimage_libsixel", "testimage_ppmtosixel"];

        for test_image in &test_images {
            // Load Sixel data.
            let mut sixel = {
                let mut path = images_dir.join(test_image);
                path.set_extension("sixel");
                fs::read(path).unwrap()
            };

            // Remove DCS sequence from Sixel data.
            let dcs_end = sixel.iter().position(|&byte| byte == b'q').unwrap();
            sixel.drain(..=dcs_end);

            // Remove ST, which can be either "1B 5C" or "9C". To simplify the
            // code, we assume that any ESC byte is the start of the ST.
            if let Some(pos) = sixel.iter().position(|&b| b == 0x1B || b == 0x9C) {
                sixel.truncate(pos);
            }

            // Parse the data and get the GraphicData item.
            let mut parser = Parser::default();
            for byte in sixel {
                parser.put(byte).unwrap();
            }

            let graphics = parser.finish().unwrap().0;

            assert_eq!(graphics.width, 64);
            assert_eq!(graphics.height, 64);

            // Read the RGBA stream generated by ImageMagick and compare it
            // with our picture.
            let expected_rgba = {
                let mut path = images_dir.join(test_image);
                path.set_extension("rgba");
                fs::read(path).unwrap()
            };

            assert_eq!(graphics.pixels, expected_rgba);
        }
    }
}
