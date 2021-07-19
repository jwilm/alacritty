use std::fmt::{self, Formatter};
use std::os::raw::c_ulong;

use glutin::window::Fullscreen;
use log::error;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use alacritty_config_derive::ConfigDeserialize;
use alacritty_terminal::config::{Percentage, LOG_TARGET_CONFIG};
use alacritty_terminal::index::Column;

use crate::config::ui_config::Delta;

/// Default Alacritty name, used for window title and class.
pub const DEFAULT_NAME: &str = "Alacritty";

#[derive(ConfigDeserialize, Debug, Clone, PartialEq)]
pub struct WindowConfig {
    /// Initial position.
    pub position: Option<Delta<i32>>,

    /// Draw the window with title bar / borders.
    pub decorations: Decorations,

    /// Startup mode.
    pub startup_mode: StartupMode,

    /// XEmbed parent.
    #[config(skip)]
    pub embed: Option<c_ulong>,

    /// GTK theme variant.
    pub gtk_theme_variant: Option<String>,

    /// Spread out additional padding evenly.
    pub dynamic_padding: bool,

    /// Use dynamic title.
    pub dynamic_title: bool,

    /// Window title.
    pub title: String,

    /// Window class.
    pub class: Class,

    /// Background opacity from 0.0 to 1.0.
    pub background_opacity: Percentage,

    /// Pixel padding.
    padding: Delta<u8>,

    /// Initial dimensions.
    dimensions: Dimensions,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            dynamic_title: true,
            title: DEFAULT_NAME.into(),
            position: Default::default(),
            decorations: Default::default(),
            startup_mode: Default::default(),
            embed: Default::default(),
            gtk_theme_variant: Default::default(),
            dynamic_padding: Default::default(),
            class: Default::default(),
            background_opacity: Default::default(),
            padding: Default::default(),
            dimensions: Default::default(),
        }
    }
}

impl WindowConfig {
    #[inline]
    pub fn dimensions(&self) -> Option<Dimensions> {
        if self.dimensions.columns.0 != 0
            && self.dimensions.lines != 0
            && self.startup_mode != StartupMode::Maximized
        {
            Some(self.dimensions)
        } else {
            None
        }
    }

    #[inline]
    pub fn padding(&self, dpr: f64) -> (f32, f32) {
        let padding_x = (f32::from(self.padding.x) * dpr as f32).floor();
        let padding_y = (f32::from(self.padding.y) * dpr as f32).floor();
        (padding_x, padding_y)
    }

    #[inline]
    pub fn fullscreen(&self) -> Option<Fullscreen> {
        if self.startup_mode == StartupMode::Fullscreen {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        }
    }

    #[inline]
    pub fn maximized(&self) -> bool {
        self.startup_mode == StartupMode::Maximized
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum StartupMode {
    Windowed,
    Maximized,
    Fullscreen,
    #[cfg(target_os = "macos")]
    SimpleFullscreen,
}

impl Default for StartupMode {
    fn default() -> StartupMode {
        StartupMode::Windowed
    }
}

#[derive(ConfigDeserialize, Debug, Copy, Clone, PartialEq, Eq)]
pub enum Decorations {
    Full,
    #[cfg(target_os = "macos")]
    Transparent,
    #[cfg(target_os = "macos")]
    Buttonless,
    None,
}

impl Default for Decorations {
    fn default() -> Decorations {
        Decorations::Full
    }
}

/// Window Dimensions.
///
/// Newtype to avoid passing values incorrectly.
#[derive(ConfigDeserialize, Default, Debug, Copy, Clone, PartialEq, Eq)]
pub struct Dimensions {
    /// Window width in character columns.
    pub columns: Column,

    /// Window Height in character lines.
    pub lines: usize,
}

/// Window class hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub instance: String,
    pub general: String,
}

impl Default for Class {
    fn default() -> Self {
        Self { instance: DEFAULT_NAME.into(), general: DEFAULT_NAME.into() }
    }
}

impl<'de> Deserialize<'de> for Class {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ClassVisitor;
        impl<'a> Visitor<'a> for ClassVisitor {
            type Value = Class;

            fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Self::Value { instance: value.into(), ..Self::Value::default() })
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'a>,
            {
                let mut class = Self::Value::default();

                while let Some((key, value)) = map.next_entry::<String, serde_yaml::Value>()? {
                    match key.as_str() {
                        "instance" => match String::deserialize(value) {
                            Ok(instance) => class.instance = instance,
                            Err(err) => {
                                error!(
                                    target: LOG_TARGET_CONFIG,
                                    "Config error: class.instance: {}", err
                                );
                            },
                        },
                        "general" => match String::deserialize(value) {
                            Ok(general) => class.general = general,
                            Err(err) => {
                                error!(
                                    target: LOG_TARGET_CONFIG,
                                    "Config error: class.instance: {}", err
                                );
                            },
                        },
                        _ => (),
                    }
                }

                Ok(class)
            }
        }

        deserializer.deserialize_any(ClassVisitor)
    }
}
