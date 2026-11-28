// Folio: this module is Folio's, not moxcms's (Folio's SPEC §2.4, #312).
//
// The float functions moxcms took from `pxfm` at run time, under pxfm's
// names, computed with the `libm` crate instead. Several of pxfm's functions
// pick an AVX+FMA or a generic path by CPU at run time and by target at
// compile time, and its `dirty_*` functions are approximations whose
// results differ with fused multiply-adds, so the same colour could convert
// to other bits on another machine. `libm` is plain Rust with IEEE
// operations only, so it gives the same bits everywhere. `defaults.rs`
// still uses pxfm's `const fn`s, which the compiler evaluates, the same on
// every host.

pub(crate) use libm::{
    atan2f as f_atan2f, cbrtf as f_cbrtf, exp as f_exp, exp2 as f_exp2, exp2f as f_exp2f,
    exp10 as f_exp10, exp10f as f_exp10f, expf as f_expf, hypotf as f_hypotf, log as f_log,
    log2 as f_log2, log2f as f_log2f, log10 as f_log10, log10f as f_log10f, logf as f_logf,
    pow as f_pow, powf as f_powf, sincosf as f_sincosf, sinf as f_sinf,
};

/// `sqrt(x² + y² + z²)`: the squares are exact in `f64`, the sum and the
/// square root rounded as IEEE 754 specifies.
#[inline]
pub(crate) fn f_hypot3f(x: f32, y: f32, z: f32) -> f32 {
    let (x, y, z) = (x as f64, y as f64, z as f64);
    libm::sqrt(x * x + y * y + z * z) as f32
}

/// pxfm's `dirty_powf`: `|d|^n`, negated for a negative `d` and an odd
/// integer part of `n`.
#[inline]
pub(crate) fn dirty_powf(d: f32, n: f32) -> f32 {
    let c = libm::powf(d.abs(), n);
    if d < 0.0 && (n as i32) % 2 != 0 {
        -c
    } else {
        c
    }
}

/// The powers and cube roots of `Rgb<T>`'s methods, which called std's
/// through `num_traits::Float`.
pub trait LibmFloat: Copy {
    /// The cube root, with `libm`.
    fn libm_cbrt(self) -> Self;
    /// `self` to the power `n`, with `libm`.
    fn libm_powf(self, n: Self) -> Self;
}

impl LibmFloat for f32 {
    #[inline]
    fn libm_cbrt(self) -> Self {
        libm::cbrtf(self)
    }

    #[inline]
    fn libm_powf(self, n: Self) -> Self {
        libm::powf(self, n)
    }
}

impl LibmFloat for f64 {
    #[inline]
    fn libm_cbrt(self) -> Self {
        libm::cbrt(self)
    }

    #[inline]
    fn libm_powf(self, n: Self) -> Self {
        libm::pow(self, n)
    }
}
