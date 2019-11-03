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

//! Line and Column newtypes for strongly typed tty/grid/terminal APIs

/// Indexing types and implementations for Grid and Line
use std::cmp::{Ord, Ordering};
use std::fmt;
use std::ops::{self, Add, AddAssign, Deref, Range, RangeInclusive, Sub, SubAssign};

use serde::{Deserialize, Serialize};

use crate::term::RenderableCell;

/// The side of a cell
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Side {
    Left,
    Right,
}

/// Index in the grid using row, column notation
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, PartialOrd)]
pub struct Point<L = Line> {
    pub line: L,
    pub col: Column,
}

impl<L> Point<L> {
    pub fn new(line: L, col: Column) -> Point<L> {
        Point { line, col }
    }
}

impl Ord for Point {
    fn cmp(&self, other: &Point) -> Ordering {
        use std::cmp::Ordering::*;
        match (self.line.cmp(&other.line), self.col.cmp(&other.col)) {
            (Equal, Equal) => Equal,
            (Equal, ord) | (ord, Equal) => ord,
            (Less, _) => Less,
            (Greater, _) => Greater,
        }
    }
}

impl From<Point<usize>> for Point<isize> {
    fn from(point: Point<usize>) -> Self {
        Point::new(point.line as isize, point.col)
    }
}

impl From<Point<usize>> for Point<Line> {
    fn from(point: Point<usize>) -> Self {
        Point::new(Line(point.line), point.col)
    }
}

impl From<Point<isize>> for Point<usize> {
    fn from(point: Point<isize>) -> Self {
        Point::new(point.line as usize, point.col)
    }
}

impl From<Point> for Point<usize> {
    fn from(point: Point) -> Self {
        Point::new(point.line.0, point.col)
    }
}

impl From<RenderableCell> for Point<Line> {
    fn from(cell: RenderableCell) -> Self {
        Point::new(cell.line, cell.column)
    }
}

/// A line
///
/// Newtype to avoid passing values incorrectly
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Line(pub usize);

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A column
///
/// Newtype to avoid passing values incorrectly
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Column(pub usize);

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A linear index
///
/// Newtype to avoid passing values incorrectly
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Linear(pub usize);

impl Linear {
    pub fn new(columns: Column, column: Column, line: Line) -> Self {
        Linear(line.0 * columns.0 + column.0)
    }

    pub fn from_point(columns: Column, point: Point<usize>) -> Self {
        Linear(point.line * columns.0 + point.col.0)
    }

    pub fn into_point(self, columns: Column) -> Point<Line> {
        let line = Line(self.0 / columns.0);
        let col = Column(self.0 % columns.0);
        Point { col, line }
    }
}

impl fmt::Display for Linear {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Linear({})", self.0)
    }
}

// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
//
// implements binary operators "&T op U", "T op &U", "&T op &U"
// based on "T op U" where T and U are expected to be `Copy`able
macro_rules! forward_ref_binop {
    (impl $imp:ident, $method:ident for $t:ty, $u:ty) => {
        impl<'a> $imp<$u> for &'a $t {
            type Output = <$t as $imp<$u>>::Output;

            #[inline]
            fn $method(self, other: $u) -> <$t as $imp<$u>>::Output {
                $imp::$method(*self, other)
            }
        }

        impl<'a> $imp<&'a $u> for $t {
            type Output = <$t as $imp<$u>>::Output;

            #[inline]
            fn $method(self, other: &'a $u) -> <$t as $imp<$u>>::Output {
                $imp::$method(self, *other)
            }
        }

        impl<'a, 'b> $imp<&'a $u> for &'b $t {
            type Output = <$t as $imp<$u>>::Output;

            #[inline]
            fn $method(self, other: &'a $u) -> <$t as $imp<$u>>::Output {
                $imp::$method(*self, *other)
            }
        }
    };
}

/// Macro for deriving deref
macro_rules! deref {
    ($ty:ty, $target:ty) => {
        impl Deref for $ty {
            type Target = $target;

            #[inline]
            fn deref(&self) -> &$target {
                &self.0
            }
        }
    };
}

macro_rules! add {
    ($ty:ty, $construct:expr) => {
        impl ops::Add<$ty> for $ty {
            type Output = $ty;

            #[inline]
            fn add(self, rhs: $ty) -> $ty {
                $construct(self.0 + rhs.0)
            }
        }
    };
}

macro_rules! sub {
    ($ty:ty, $construct:expr) => {
        impl ops::Sub<$ty> for $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: $ty) -> $ty {
                $construct(self.0 - rhs.0)
            }
        }

        impl<'a> ops::Sub<$ty> for &'a $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: $ty) -> $ty {
                $construct(self.0 - rhs.0)
            }
        }

        impl<'a> ops::Sub<&'a $ty> for $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: &'a $ty) -> $ty {
                $construct(self.0 - rhs.0)
            }
        }

        impl<'a, 'b> ops::Sub<&'a $ty> for &'b $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: &'a $ty) -> $ty {
                $construct(self.0 - rhs.0)
            }
        }
    };
}

/// This exists because we can't implement Iterator on Range
/// and the existing impl needs the unstable Step trait
/// This should be removed and replaced with a Step impl
/// in the ops macro when `step_by` is stabilized
pub struct IndexRange<T>(pub Range<T>);

impl<T> From<Range<T>> for IndexRange<T> {
    fn from(from: Range<T>) -> Self {
        IndexRange(from)
    }
}

// can be removed if range_contains is stabilized
pub trait Contains {
    type Content;
    fn contains_(&self, item: Self::Content) -> bool;
}

impl<T: PartialOrd<T>> Contains for Range<T> {
    type Content = T;

    fn contains_(&self, item: Self::Content) -> bool {
        (self.start <= item) && (item < self.end)
    }
}

impl<T: PartialOrd<T>> Contains for RangeInclusive<T> {
    type Content = T;

    fn contains_(&self, item: Self::Content) -> bool {
        (self.start() <= &item) && (&item <= self.end())
    }
}

macro_rules! ops {
    ($ty:ty, $construct:expr) => {
        add!($ty, $construct);
        sub!($ty, $construct);
        deref!($ty, usize);
        forward_ref_binop!(impl Add, add for $ty, $ty);

        impl $ty {
            #[inline]
            fn steps_between(start: $ty, end: $ty, by: $ty) -> Option<usize> {
                if by == $construct(0) { return None; }
                if start < end {
                    // Note: We assume $t <= usize here
                    let diff = (end - start).0;
                    let by = by.0;
                    if diff % by > 0 {
                        Some(diff / by + 1)
                    } else {
                        Some(diff / by)
                    }
                } else {
                    Some(0)
                }
            }

            #[inline]
            fn steps_between_by_one(start: $ty, end: $ty) -> Option<usize> {
                Self::steps_between(start, end, $construct(1))
            }
        }

        impl Iterator for IndexRange<$ty> {
            type Item = $ty;
            #[inline]
            fn next(&mut self) -> Option<$ty> {
                if self.0.start < self.0.end {
                    let old = self.0.start;
                    self.0.start = old + 1;
                    Some(old)
                } else {
                    None
                }
            }
            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                match Self::Item::steps_between_by_one(self.0.start, self.0.end) {
                    Some(hint) => (hint, Some(hint)),
                    None => (0, None)
                }
            }
        }

        impl DoubleEndedIterator for IndexRange<$ty> {
            #[inline]
            fn next_back(&mut self) -> Option<$ty> {
                if self.0.start < self.0.end {
                    let new = self.0.end - 1;
                    self.0.end = new;
                    Some(new)
                } else {
                    None
                }
            }
        }
        impl AddAssign<$ty> for $ty {
            #[inline]
            fn add_assign(&mut self, rhs: $ty) {
                self.0 += rhs.0
            }
        }

        impl SubAssign<$ty> for $ty {
            #[inline]
            fn sub_assign(&mut self, rhs: $ty) {
                self.0 -= rhs.0
            }
        }

        impl AddAssign<usize> for $ty {
            #[inline]
            fn add_assign(&mut self, rhs: usize) {
                self.0 += rhs
            }
        }

        impl SubAssign<usize> for $ty {
            #[inline]
            fn sub_assign(&mut self, rhs: usize) {
                self.0 -= rhs
            }
        }

        impl From<usize> for $ty {
            #[inline]
            fn from(val: usize) -> $ty {
                $construct(val)
            }
        }

        impl Add<usize> for $ty {
            type Output = $ty;

            #[inline]
            fn add(self, rhs: usize) -> $ty {
                $construct(self.0 + rhs)
            }
        }

        impl Sub<usize> for $ty {
            type Output = $ty;

            #[inline]
            fn sub(self, rhs: usize) -> $ty {
                $construct(self.0 - rhs)
            }
        }
    }
}

ops!(Line, Line);
ops!(Column, Column);
ops!(Linear, Linear);

#[cfg(test)]
mod tests {
    use super::{Column, Line, Point};

    #[test]
    fn location_ordering() {
        assert!(Point::new(Line(0), Column(0)) == Point::new(Line(0), Column(0)));
        assert!(Point::new(Line(1), Column(0)) > Point::new(Line(0), Column(0)));
        assert!(Point::new(Line(0), Column(1)) > Point::new(Line(0), Column(0)));
        assert!(Point::new(Line(1), Column(1)) > Point::new(Line(0), Column(0)));
        assert!(Point::new(Line(1), Column(1)) > Point::new(Line(0), Column(1)));
        assert!(Point::new(Line(1), Column(1)) > Point::new(Line(1), Column(0)));
    }
}
