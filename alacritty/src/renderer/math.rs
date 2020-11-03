#[derive(Debug, Copy, Clone)]
pub struct Vec2<T: Copy> {
    pub x: T,
    pub y: T,
}

impl<T: Copy> Vec2<T> {
    pub fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl<T: Copy + Ord> Vec2<T> {
    pub fn min(self, other: Self) -> Self {
        Self { x: std::cmp::min(self.x, other.x), y: std::cmp::min(self.y, other.y) }
    }
}

impl<T: std::ops::Add<Output = T> + Copy> std::ops::Add for Vec2<T> {
    type Output = Vec2<T>;

    fn add(self, rhs: Self) -> Self {
        Self { x: self.x + rhs.x, y: self.y + rhs.y }
    }
}

impl<T: std::ops::Add<Output = T> + Copy> std::ops::Add<T> for Vec2<T> {
    type Output = Vec2<T>;

    fn add(self, rhs: T) -> Self {
        Self { x: self.x + rhs, y: self.y + rhs }
    }
}

impl<T: std::ops::Sub<Output = T> + Copy> std::ops::Sub for Vec2<T> {
    type Output = Vec2<T>;

    fn sub(self, rhs: Self) -> Self {
        Self { x: self.x - rhs.x, y: self.y - rhs.y }
    }
}

impl<T: std::ops::Sub<Output = T> + Copy> std::ops::Sub<T> for Vec2<T> {
    type Output = Vec2<T>;

    fn sub(self, rhs: T) -> Self {
        Self { x: self.x - rhs, y: self.y - rhs }
    }
}

impl<T: std::ops::Mul<Output = T> + Copy> std::ops::Mul for Vec2<T> {
    type Output = Vec2<T>;

    fn mul(self, rhs: Self) -> Self {
        Self { x: self.x * rhs.x, y: self.y * rhs.y }
    }
}

impl<T: std::ops::Div<Output = T> + Copy> std::ops::Div<Vec2<T>> for Vec2<T> {
    type Output = Self;

    fn div(self, rhs: Self) -> Self::Output {
        Self::Output { x: self.x / rhs.x, y: self.y / rhs.y }
    }
}

impl<T: std::ops::Div<Output = T> + Copy> std::ops::Div<T> for Vec2<T> {
    type Output = Vec2<T>;

    fn div(self, rhs: T) -> Self {
        Self { x: self.x / rhs, y: self.y / rhs }
    }
}

impl<T: Copy> From<T> for Vec2<T> {
    fn from(v: T) -> Self {
        Self { x: v, y: v }
    }
}
