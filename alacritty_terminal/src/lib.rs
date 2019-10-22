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
//! Alacritty - The GPU Enhanced Terminal
#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use, clippy::wrong_pub_self_convention)]
#![cfg_attr(feature = "nightly", feature(core_intrinsics))]
#![cfg_attr(all(test, feature = "bench"), feature(test))]

#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

pub mod ansi;
pub mod clipboard;
pub mod config;
mod cursor;
pub mod event;
pub mod event_loop;
pub mod grid;
pub mod index;
pub mod locale;
pub mod message_bar;
pub mod meter;
pub mod panic;
pub mod renderer;
pub mod selection;
pub mod sync;
pub mod term;
pub mod text_run;
pub mod tty;
pub mod url;
pub mod util;

pub use crate::grid::Grid;
pub use crate::term::Term;

pub mod gl {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}
