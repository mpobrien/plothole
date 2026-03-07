use num_traits::{Num, Signed, Zero};
use std::ops::Add;
use std::ops::Mul;
use std::ops::Neg;
use std::ops::Sub;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Glyph<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    pub left: T,
    pub right: T,
    pub paths: Vec<Path<T>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Vec2d<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    pub x: T,
    pub y: T,
}

impl<T> Vec2d<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    pub fn new(x: T, y: T) -> Self {
        Self { x, y }
    }

    pub fn distance(&self, other: &Vec2d<T>) -> f64 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        let z = ((dx * dx) + (dy * dy)).to_f64().unwrap();
        z.sqrt()
    }
}

impl<T> Vec2d<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    pub fn tuple(&self) -> (T, T) {
        (self.x, self.y)
    }
}

impl<T> Add for &Vec2d<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    type Output = Vec2d<T>;

    fn add(self, rhs: &Vec2d<T>) -> Self::Output {
        Vec2d {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Path<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    points: Vec<Vec2d<T>>,
}

impl<T> Path<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    pub fn empty() -> Self {
        Self { points: vec![] }
    }

    pub fn push(&mut self, point: Vec2d<T>) {
        self.points.push(point);
    }

    pub fn start(&self) -> &Vec2d<T> {
        &self.points.first().unwrap()
    }
    pub fn end(&self) -> &Vec2d<T> {
        &self.points.last().unwrap()
    }

    pub fn new(points: Vec<Vec2d<T>>) -> Self {
        Self { points }
    }

    pub fn points<'a>(&'a self) -> &'a Vec<Vec2d<T>> {
        &self.points
    }
}

pub trait ToF64 {
    fn to_f64(self) -> Option<f64>;
}

impl ToF64 for f64 {
    fn to_f64(self) -> Option<f64> {
        Some(self)
    }
}

impl ToF64 for f32 {
    fn to_f64(self) -> Option<f64> {
        Some(self as f64)
    }
}

impl ToF64 for i32 {
    fn to_f64(self) -> Option<f64> {
        Some(self as f64)
    }
}

pub type Font = Vec<Glyph<i32>>;

// macro_rules! v2d {
//     ( $x:expr, $y:expr ) => {{ Vec2d { x: $x, y: $y } }};
// }
//
// macro_rules! path {
//     ( $( ($x:expr, $y:expr) ),* ) => {
//         {
//             let mut _p = Vec::new();
//             $(
//                 _p.push(Vec2d{x: $x, y: $x});
//             )*
//             _p
//         }
//     };
// }
//
#[macro_export]
macro_rules! glyph {
    // Nested paths: [ [ (x,y), (x,y), ... ], [ (x,y), ... ], ... ]
    ( $left:expr, $right:expr,
      [ $( [ $( ( $x:expr, $y:expr ) ),* $(,)? ] ),* $(,)? ]
    ) => {
        $crate::font::Glyph {
            left: $left,
            right: $right,
            paths: vec![
                $(
                    $crate::font::Path::new(vec![
                        $( $crate::font::Vec2d { x: $x, y: $y } ),*
                    ])
                ),*
            ],
        }
    };

    // Single flat path: [ (x,y), (x,y), ... ] gets wrapped as one subpath
    ( $left:expr, $right:expr,
      [ $( ( $x:expr, $y:expr ) ),* $(,)? ]
    ) => {
        $crate:font::Glyph {
            left: $left,
            right: $right,
            paths: vec![
                vec![
                    $crate::font::Path::new( $( Vec2d { x: $x, y: $y } ),* )
                ]
            ],
        }
    };
}
