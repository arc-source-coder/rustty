use std::fmt;
use std::hash::{Hash, Hasher};

mod sealed {
    pub trait Sealed {}

    impl Sealed for f32 {}
    impl Sealed for f64 {}
}

pub trait Float: sealed::Sealed + Copy + PartialEq + PartialOrd {
    type Bits: Hash;

    fn is_nan(self) -> bool;
    fn canonical_bits(self) -> Self::Bits;
}

impl Float for f32 {
    type Bits = u32;

    #[inline]
    fn is_nan(self) -> bool {
        f32::is_nan(self)
    }

    #[inline]
    fn canonical_bits(self) -> Self::Bits {
        if self == 0.0 { 0 } else { self.to_bits() }
    }
}

impl Float for f64 {
    type Bits = u64;

    #[inline]
    fn is_nan(self) -> bool {
        f64::is_nan(self)
    }

    #[inline]
    fn canonical_bits(self) -> Self::Bits {
        if self == 0.0 { 0 } else { self.to_bits() }
    }
}

#[derive(PartialEq, Default, Clone, Copy)]
#[repr(transparent)]
pub struct NotNan<T: Float>(T);

// The inner value will never be NaN
impl<T: Float> Eq for NotNan<T> {}

impl<T: Float + fmt::Display> fmt::Display for NotNan<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Float> Hash for NotNan<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.canonical_bits().hash(state);
    }
}

impl<T: Float> NotNan<T> {
    #[inline]
    pub fn new(val: T) -> Option<Self> {
        if val.is_nan() { None } else { Some(Self(val)) }
    }

    #[inline]
    pub const fn get(self) -> T {
        self.0
    }
}
