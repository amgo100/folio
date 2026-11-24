//! Units and geometry: [`Abs`], [`Em`], [`Ratio`], [`Point`], [`Size`] and
//! [`Rect`].
//!
//! Every type here is plain data with total equality and a stable hash, so
//! fragments and container states that hold them can be compared for early
//! cutoff and used as cache keys (SPEC §2.3).

use std::cmp::Ordering;
use std::fmt::{self, Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::ops::{Add, Div, Mul, Neg, Sub};

/// A finite 64-bit float with total equality, ordering and a stable hash.
///
/// NaN becomes zero and `-0.0` becomes `0.0`, so equal values hash equally.
/// Infinities are kept: a region of unbounded height is infinitely tall.
#[derive(Copy, Clone, Default)]
pub struct Scalar(f64);

impl Scalar {
    /// Zero.
    pub const ZERO: Self = Self(0.0);

    /// Wraps a float, normalising NaN and negative zero to zero.
    pub const fn new(value: f64) -> Self {
        if value.is_nan() || value == 0.0 {
            Self(0.0)
        } else {
            Self(value)
        }
    }

    /// The float.
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for Scalar {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for Scalar {}

impl PartialOrd for Scalar {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scalar {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Hash for Scalar {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Debug for Scalar {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl From<f64> for Scalar {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

/// Defines a unit: a newtype over [`Scalar`] with arithmetic.
macro_rules! unit {
    ($(#[$attr:meta])* $name:ident, $suffix:literal) => {
        $(#[$attr])*
        #[derive(Copy, Clone, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(Scalar);

        impl $name {
            /// Zero.
            pub const fn zero() -> Self {
                Self(Scalar::ZERO)
            }

            /// The value as a float in the unit's own measure.
            pub const fn get(self) -> f64 {
                self.0.get()
            }

            /// The smaller of two values.
            pub fn min(self, other: Self) -> Self {
                Ord::min(self, other)
            }

            /// The larger of two values.
            pub fn max(self, other: Self) -> Self {
                Ord::max(self, other)
            }

            /// Whether the value is zero.
            pub fn is_zero(self) -> bool {
                self == Self::zero()
            }
        }

        impl Add for $name {
            type Output = Self;
            fn add(self, rhs: Self) -> Self {
                Self(Scalar::new(self.get() + rhs.get()))
            }
        }

        impl Sub for $name {
            type Output = Self;
            fn sub(self, rhs: Self) -> Self {
                Self(Scalar::new(self.get() - rhs.get()))
            }
        }

        impl Neg for $name {
            type Output = Self;
            fn neg(self) -> Self {
                Self(Scalar::new(-self.get()))
            }
        }

        impl Mul<f64> for $name {
            type Output = Self;
            fn mul(self, rhs: f64) -> Self {
                Self(Scalar::new(self.get() * rhs))
            }
        }

        impl Div<f64> for $name {
            type Output = Self;
            fn div(self, rhs: f64) -> Self {
                Self(Scalar::new(self.get() / rhs))
            }
        }

        impl Debug for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, "{:?}{}", self.get(), $suffix)
            }
        }
    };
}

unit! {
    /// An absolute length, stored in points (1/72 inch).
    Abs, "pt"
}

unit! {
    /// A length relative to the font size: `1em` is the font size.
    Em, "em"
}

unit! {
    /// A ratio, where `1.0` is 100%.
    Ratio, ""
}

impl Abs {
    /// A length in points.
    pub const fn pt(pt: f64) -> Self {
        Self(Scalar::new(pt))
    }

    /// A length in millimetres.
    pub const fn mm(mm: f64) -> Self {
        Self::pt(mm * 72.0 / 25.4)
    }

    /// A length in centimetres.
    pub const fn cm(cm: f64) -> Self {
        Self::pt(cm * 72.0 / 2.54)
    }

    /// A length in inches.
    pub const fn inches(inches: f64) -> Self {
        Self::pt(inches * 72.0)
    }

    /// An infinite length: the height of an unbounded region.
    pub const fn inf() -> Self {
        Self::pt(f64::INFINITY)
    }

    /// The length in points.
    pub const fn to_pt(self) -> f64 {
        self.get()
    }

    /// Whether the length is finite.
    pub fn is_finite(self) -> bool {
        self.get().is_finite()
    }
}

impl Em {
    /// A length in ems.
    pub const fn new(em: f64) -> Self {
        Self(Scalar::new(em))
    }

    /// The absolute length at a font size.
    pub fn at(self, font_size: Abs) -> Abs {
        Abs::pt(self.get() * font_size.to_pt())
    }
}

impl Ratio {
    /// A ratio: `Ratio::new(0.5)` is 50%.
    pub const fn new(ratio: f64) -> Self {
        Self(Scalar::new(ratio))
    }

    /// 100%.
    pub const fn one() -> Self {
        Self::new(1.0)
    }

    /// The ratio of a length.
    pub fn of(self, whole: Abs) -> Abs {
        whole * self.get()
    }
}

/// A point, relative to the top-left corner of the enclosing fragment, with
/// `y` growing downwards.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Point {
    /// The horizontal coordinate.
    pub x: Abs,
    /// The vertical coordinate.
    pub y: Abs,
}

impl Point {
    /// The origin.
    pub const fn zero() -> Self {
        Self::new(Abs::zero(), Abs::zero())
    }

    /// A point from its coordinates.
    pub const fn new(x: Abs, y: Abs) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

/// A two-dimensional size.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Size {
    /// The width.
    pub width: Abs,
    /// The height.
    pub height: Abs,
}

impl Size {
    /// A zero size.
    pub const fn zero() -> Self {
        Self::new(Abs::zero(), Abs::zero())
    }

    /// A size from its width and height.
    pub const fn new(width: Abs, height: Abs) -> Self {
        Self { width, height }
    }
}

/// An axis-aligned rectangle.
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
pub struct Rect {
    /// The top-left corner.
    pub origin: Point,
    /// The size.
    pub size: Size,
}

impl Rect {
    /// A rectangle from its top-left corner and size.
    pub const fn new(origin: Point, size: Size) -> Self {
        Self { origin, size }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;

    use super::*;

    fn hash(value: impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn scalars_are_totally_equal() {
        assert_eq!(Scalar::new(f64::NAN), Scalar::ZERO);
        assert_eq!(Scalar::new(-0.0), Scalar::ZERO);
        assert_eq!(hash(Scalar::new(-0.0)), hash(Scalar::ZERO));
        assert!(Scalar::new(1.0) < Scalar::new(f64::INFINITY));
    }

    #[test]
    fn units() {
        assert_eq!(Abs::inches(1.0), Abs::pt(72.0));
        assert!((Abs::mm(25.4).to_pt() - 72.0).abs() < 1e-9);
        assert_eq!(Em::new(0.5).at(Abs::pt(12.0)), Abs::pt(6.0));
        assert_eq!(Ratio::new(0.25).of(Abs::pt(8.0)), Abs::pt(2.0));
        assert_eq!(Abs::pt(3.0) - Abs::pt(3.0), Abs::zero());
        assert!(!Abs::inf().is_finite());
        assert_eq!(format!("{:?}", Abs::pt(1.5)), "1.5pt");
    }
}
