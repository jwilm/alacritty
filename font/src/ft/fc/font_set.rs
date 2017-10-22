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
use std::ops::Deref;

use foreign_types::{ForeignType, ForeignTypeRef};

use super::{ConfigRef, PatternRef, ObjectSetRef};

use super::ffi::{FcFontSetList, FcFontSetDestroy, FcFontSet};

foreign_type! {
    type CType = FcFontSet;
    fn drop = FcFontSetDestroy;
    /// Wraps an FcFontSet instance (owned)
    pub struct FontSet;
    /// Wraps an FcFontSet reference (borrowed)
    pub struct FontSetRef;
}

impl FontSet {
    pub fn list(
        config: &ConfigRef,
        source: &mut FontSetRef,
        pattern: &PatternRef,
        objects: &ObjectSetRef
    ) -> FontSet {
        let raw = unsafe {
            FcFontSetList(
                config.as_ptr(),
                &mut source.as_ptr(),
                1 /* nsets */,
                pattern.as_ptr(),
                objects.as_ptr(),
            )
        };
        FontSet(raw)
    }
}

/// Iterator over a font set
pub struct Iter<'a> {
    font_set: &'a FontSetRef,
    num_fonts: usize,
    current: usize,
}

impl<'a> IntoIterator for &'a FontSet {
    type Item = &'a PatternRef;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        let num_fonts = unsafe {
            (*self.as_ptr()).nfont as isize
        };

        info!("num fonts = {}", num_fonts);

        Iter {
            font_set: self.deref(),
            num_fonts: num_fonts as _,
            current: 0,
        }
    }
}

impl<'a> IntoIterator for &'a FontSetRef {
    type Item = &'a PatternRef;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        let num_fonts = unsafe {
            (*self.as_ptr()).nfont as isize
        };

        info!("num fonts = {}", num_fonts);

        Iter {
            font_set: self,
            num_fonts: num_fonts as _,
            current: 0,
        }
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a PatternRef;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current == self.num_fonts {
            None
        } else {
            let pattern = unsafe {
                let ptr = *(*self.font_set.as_ptr()).fonts.offset(self.current as isize);
                PatternRef::from_ptr(ptr)
            };

            self.current += 1;
            Some(pattern)
        }
    }
}
