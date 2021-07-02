use serde::de::Error as SerdeError;
use serde::{Deserialize, Deserializer};

use alacritty_config_derive::ConfigDeserialize;
use alacritty_terminal::term::color::{CellRgb, Rgb};

#[derive(ConfigDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    pub primary: PrimaryColors,
    pub cursor: InvertedCellColors,
    pub vi_mode_cursor: InvertedCellColors,
    pub selection: InvertedCellColors,
    pub normal: NormalColors,
    pub bright: BrightColors,
    pub dim: Option<DimColors>,
    pub indexed_colors: Vec<IndexedColor>,
    pub search: SearchColors,
    pub line_indicator: LineIndicatorColors,
    pub hints: HintColors,
    pub opaque_background_colors: bool,
}

impl Colors {
    pub fn search_bar_foreground(&self) -> Rgb {
        self.search.bar.foreground.unwrap_or(self.primary.background)
    }

    pub fn search_bar_background(&self) -> Rgb {
        self.search.bar.background.unwrap_or(self.primary.foreground)
    }
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            primary: Default::default(),
            cursor: Default::default(),
            vi_mode_cursor: Default::default(),
            selection: Default::default(),
            normal: Default::default(),
            bright: Default::default(),
            dim: Default::default(),
            indexed_colors: Default::default(),
            search: Default::default(),
            line_indicator: Default::default(),
            hints: Default::default(),
            opaque_background_colors: true,
        }
    }
}

#[derive(ConfigDeserialize, Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct LineIndicatorColors {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
}

#[derive(ConfigDeserialize, Default, Copy, Clone, Debug, PartialEq, Eq)]
pub struct HintColors {
    pub start: HintStartColors,
    pub end: HintEndColors,
}

#[derive(ConfigDeserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct HintStartColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for HintStartColors {
    fn default() -> Self {
        Self {
            foreground: CellRgb::Rgb(Rgb { r: 0x1d, g: 0x1f, b: 0x21 }),
            background: CellRgb::Rgb(Rgb { r: 0xe9, g: 0xff, b: 0x5e }),
        }
    }
}

#[derive(ConfigDeserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct HintEndColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for HintEndColors {
    fn default() -> Self {
        Self {
            foreground: CellRgb::Rgb(Rgb { r: 0xe9, g: 0xff, b: 0x5e }),
            background: CellRgb::Rgb(Rgb { r: 0x1d, g: 0x1f, b: 0x21 }),
        }
    }
}

#[derive(Deserialize, Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct IndexedColor {
    pub color: Rgb,

    index: ColorIndex,
}

impl IndexedColor {
    #[inline]
    pub fn index(&self) -> u8 {
        self.index.0
    }
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
struct ColorIndex(u8);

impl<'de> Deserialize<'de> for ColorIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let index = u8::deserialize(deserializer)?;

        if index < 16 {
            Err(SerdeError::custom(
                "Config error: indexed_color's index is {}, but a value bigger than 15 was \
                 expected; ignoring setting",
            ))
        } else {
            Ok(Self(index))
        }
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct InvertedCellColors {
    #[config(alias = "text")]
    pub foreground: CellRgb,
    #[config(alias = "cursor")]
    pub background: CellRgb,
}

impl Default for InvertedCellColors {
    fn default() -> Self {
        Self { foreground: CellRgb::CellBackground, background: CellRgb::CellForeground }
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct SearchColors {
    pub focused_match: FocusedMatchColors,
    pub matches: MatchColors,
    bar: BarColors,
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct FocusedMatchColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for FocusedMatchColors {
    fn default() -> Self {
        Self {
            background: CellRgb::Rgb(Rgb { r: 0x00, g: 0x00, b: 0x00 }),
            foreground: CellRgb::Rgb(Rgb { r: 0xff, g: 0xff, b: 0xff }),
        }
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub struct MatchColors {
    pub foreground: CellRgb,
    pub background: CellRgb,
}

impl Default for MatchColors {
    fn default() -> Self {
        Self {
            background: CellRgb::Rgb(Rgb { r: 0xff, g: 0xff, b: 0xff }),
            foreground: CellRgb::Rgb(Rgb { r: 0x00, g: 0x00, b: 0x00 }),
        }
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct BarColors {
    foreground: Option<Rgb>,
    background: Option<Rgb>,
}

#[derive(ConfigDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct PrimaryColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub bright_foreground: Option<Rgb>,
    pub dim_foreground: Option<Rgb>,
}

impl Default for PrimaryColors {
    fn default() -> Self {
        PrimaryColors {
            background: Rgb { r: 0x1d, g: 0x1f, b: 0x21 },
            foreground: Rgb { r: 0xc5, g: 0xc8, b: 0xc6 },
            bright_foreground: Default::default(),
            dim_foreground: Default::default(),
        }
    }
}

#[derive(ConfigDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct NormalColors {
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
}

impl Default for NormalColors {
    fn default() -> Self {
        NormalColors {
            black: Rgb { r: 0x1d, g: 0x1f, b: 0x21 },
            red: Rgb { r: 0xcc, g: 0x66, b: 0x66 },
            green: Rgb { r: 0xb5, g: 0xbd, b: 0x68 },
            yellow: Rgb { r: 0xf0, g: 0xc6, b: 0x74 },
            blue: Rgb { r: 0x81, g: 0xa2, b: 0xbe },
            magenta: Rgb { r: 0xb2, g: 0x94, b: 0xbb },
            cyan: Rgb { r: 0x8a, g: 0xbe, b: 0xb7 },
            white: Rgb { r: 0xc5, g: 0xc8, b: 0xc6 },
        }
    }
}

#[derive(ConfigDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct BrightColors {
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
}

impl Default for BrightColors {
    fn default() -> Self {
        BrightColors {
            black: Rgb { r: 0x66, g: 0x66, b: 0x66 },
            red: Rgb { r: 0xd5, g: 0x4e, b: 0x53 },
            green: Rgb { r: 0xb9, g: 0xca, b: 0x4a },
            yellow: Rgb { r: 0xe7, g: 0xc5, b: 0x47 },
            blue: Rgb { r: 0x7a, g: 0xa6, b: 0xda },
            magenta: Rgb { r: 0xc3, g: 0x97, b: 0xd8 },
            cyan: Rgb { r: 0x70, g: 0xc0, b: 0xb1 },
            white: Rgb { r: 0xea, g: 0xea, b: 0xea },
        }
    }
}

#[derive(ConfigDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct DimColors {
    pub black: Rgb,
    pub red: Rgb,
    pub green: Rgb,
    pub yellow: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub cyan: Rgb,
    pub white: Rgb,
}

impl Default for DimColors {
    fn default() -> Self {
        DimColors {
            black: Rgb { r: 0x13, g: 0x14, b: 0x15 },
            red: Rgb { r: 0x86, g: 0x43, b: 0x43 },
            green: Rgb { r: 0x77, g: 0x7c, b: 0x44 },
            yellow: Rgb { r: 0x9e, g: 0x82, b: 0x4c },
            blue: Rgb { r: 0x55, g: 0x6a, b: 0x7d },
            magenta: Rgb { r: 0x75, g: 0x61, b: 0x7b },
            cyan: Rgb { r: 0x5b, g: 0x7d, b: 0x78 },
            white: Rgb { r: 0x82, g: 0x84, b: 0x82 },
        }
    }
}
