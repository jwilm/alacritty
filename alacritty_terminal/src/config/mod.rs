use std::cmp::max;
use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use alacritty_config_derive::ConfigDeserialize;

mod scrolling;

use crate::ansi::{CursorShape, CursorStyle};

pub use crate::config::scrolling::Scrolling;

pub const LOG_TARGET_CONFIG: &str = "alacritty_config_derive";
const MIN_BLINK_INTERVAL: u64 = 10;

pub type MockConfig = Config<HashMap<String, serde_yaml::Value>>;

/// Top-level config type.
#[derive(ConfigDeserialize, Debug, PartialEq)]
pub struct Config<T> {
    /// TERM env variable.
    pub env: HashMap<String, String>,

    pub selection: Selection,

    /// Path to a shell program to run on startup.
    pub shell: Option<Program>,

    /// How much scrolling history to keep.
    pub scrolling: Scrolling,

    /// Cursor configuration.
    pub cursor: Cursor,

    /// Shell startup directory.
    pub working_directory: Option<PathBuf>,

    /// Reflow terminal content on resize.
    pub reflow: bool,

    /// Additional configuration options not directly required by the terminal.
    #[config(flatten)]
    pub ui_config: T,

    /// Remain open after child process exits.
    #[config(skip)]
    pub hold: bool,
}

impl<T: Default> Default for Config<T> {
    fn default() -> Self {
        Self {
            reflow: true,
            env: Default::default(),
            selection: Default::default(),
            shell: Default::default(),
            scrolling: Default::default(),
            cursor: Default::default(),
            working_directory: Default::default(),
            ui_config: Default::default(),
            hold: Default::default(),
        }
    }
}

#[derive(ConfigDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub semantic_escape_chars: String,
    pub save_to_clipboard: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            semantic_escape_chars: String::from(",│`|:\"' ()[]{}<>\t"),
            save_to_clipboard: Default::default(),
        }
    }
}

#[derive(ConfigDeserialize, Copy, Clone, Debug, PartialEq)]
pub struct Cursor {
    pub style: ConfigCursorStyle,
    pub vi_mode_style: Option<ConfigCursorStyle>,
    pub unfocused_hollow: bool,

    thickness: Percentage,
    blink_interval: u64,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            thickness: Percentage(0.15),
            unfocused_hollow: true,
            blink_interval: 750,
            style: Default::default(),
            vi_mode_style: Default::default(),
        }
    }
}

impl Cursor {
    #[inline]
    pub fn thickness(self) -> f32 {
        self.thickness.as_f32()
    }

    #[inline]
    pub fn style(self) -> CursorStyle {
        self.style.into()
    }

    #[inline]
    pub fn vi_mode_style(self) -> Option<CursorStyle> {
        self.vi_mode_style.map(From::from)
    }

    #[inline]
    pub fn blink_interval(self) -> u64 {
        max(self.blink_interval, MIN_BLINK_INTERVAL)
    }
}

#[derive(Deserialize, Debug, Copy, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum ConfigCursorStyle {
    Shape(CursorShape),
    WithBlinking {
        #[serde(default)]
        shape: CursorShape,
        #[serde(default)]
        blinking: CursorBlinking,
    },
}

impl Default for ConfigCursorStyle {
    fn default() -> Self {
        Self::Shape(CursorShape::default())
    }
}

impl ConfigCursorStyle {
    /// Check if blinking is force enabled/disabled.
    pub fn blinking_override(&self) -> Option<bool> {
        match self {
            Self::Shape(_) => None,
            Self::WithBlinking { blinking, .. } => blinking.blinking_override(),
        }
    }
}

impl From<ConfigCursorStyle> for CursorStyle {
    fn from(config_style: ConfigCursorStyle) -> Self {
        match config_style {
            ConfigCursorStyle::Shape(shape) => Self { shape, blinking: false },
            ConfigCursorStyle::WithBlinking { shape, blinking } => {
                Self { shape, blinking: blinking.into() }
            },
        }
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum CursorBlinking {
    Never,
    Off,
    On,
    Always,
}

impl Default for CursorBlinking {
    fn default() -> Self {
        CursorBlinking::Off
    }
}

impl CursorBlinking {
    fn blinking_override(&self) -> Option<bool> {
        match self {
            Self::Never => Some(false),
            Self::Off | Self::On => None,
            Self::Always => Some(true),
        }
    }
}

impl From<CursorBlinking> for bool {
    fn from(blinking: CursorBlinking) -> bool {
        blinking == CursorBlinking::On || blinking == CursorBlinking::Always
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum Program {
    Just(String),
    WithArgs {
        program: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

impl Program {
    pub fn program(&self) -> &str {
        match self {
            Program::Just(program) => program,
            Program::WithArgs { program, .. } => program,
        }
    }

    pub fn args(&self) -> &[String] {
        match self {
            Program::Just(_) => &[],
            Program::WithArgs { args, .. } => args,
        }
    }
}

/// Wrapper around f32 that represents a percentage value between 0.0 and 1.0.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Percentage(f32);

impl Default for Percentage {
    fn default() -> Self {
        Percentage(1.0)
    }
}

impl Percentage {
    pub fn new(value: f32) -> Self {
        Percentage(if value < 0.0 {
            0.0
        } else if value > 1.0 {
            1.0
        } else {
            value
        })
    }

    pub fn as_f32(self) -> f32 {
        self.0
    }
}
