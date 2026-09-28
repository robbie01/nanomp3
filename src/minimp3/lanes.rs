//! A minimal abstraction over "one float" and "four floats" so each DSP kernel
//! is written once and instantiated for both a SIMD body and a scalar tail.
//!
//! Every operation acts lane-wise with exactly the scalar semantics (no fused
//! multiply-add, no reassociation), so vectorized kernels produce bit-identical
//! results to the scalar reference.

use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

pub(crate) trait Lanes:
    Copy
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Neg<Output = Self>
    + AddAssign
    + SubAssign
{
    fn splat(x: f32) -> Self;
    /// Loads the first `N` floats of `s`.
    fn load(s: &[f32]) -> Self;
    /// Stores into the first `N` floats of `s`.
    fn store(self, s: &mut [f32]);
    /// Loads `s[0], s[stride], s[2*stride], ...`.
    fn gather(s: &[f32], stride: usize) -> Self;
    /// Stores to `s[0], s[stride], s[2*stride], ...`.
    fn scatter(self, s: &mut [f32], stride: usize);
    /// Reverses the lane order.
    fn rev(self) -> Self;
}

impl Lanes for f32 {
    #[inline(always)]
    fn splat(x: f32) -> Self {
        x
    }
    #[inline(always)]
    fn load(s: &[f32]) -> Self {
        s[0]
    }
    #[inline(always)]
    fn store(self, s: &mut [f32]) {
        s[0] = self;
    }
    #[inline(always)]
    fn gather(s: &[f32], _stride: usize) -> Self {
        s[0]
    }
    #[inline(always)]
    fn scatter(self, s: &mut [f32], _stride: usize) {
        s[0] = self;
    }
    #[inline(always)]
    fn rev(self) -> Self {
        self
    }
}

pub(crate) use f32x4::F4;

#[cfg(feature = "wide")]
mod f32x4 {
    pub(crate) type F4 = wide::f32x4;

    impl super::Lanes for F4 {
        #[inline(always)]
        fn splat(x: f32) -> Self {
            F4::splat(x)
        }
        #[inline(always)]
        fn load(s: &[f32]) -> Self {
            F4::new(s[..4].try_into().unwrap())
        }
        #[inline(always)]
        fn store(self, s: &mut [f32]) {
            s[..4].copy_from_slice(&self.to_array());
        }
        #[inline(always)]
        fn gather(s: &[f32], stride: usize) -> Self {
            F4::new([s[0], s[stride], s[2 * stride], s[3 * stride]])
        }
        #[inline(always)]
        fn scatter(self, s: &mut [f32], stride: usize) {
            let a = self.to_array();
            s[3 * stride] = a[3];
            s[2 * stride] = a[2];
            s[stride] = a[1];
            s[0] = a[0];
        }
        #[inline(always)]
        fn rev(self) -> Self {
            let [a, b, c, d] = self.to_array();
            F4::new([d, c, b, a])
        }
    }
}

#[cfg(not(feature = "wide"))]
mod f32x4 {
    use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

    /// Four floats; LLVM turns these element-wise loops into SIMD on its own.
    #[derive(Copy, Clone)]
    #[repr(align(16))]
    pub(crate) struct F4([f32; 4]);

    impl F4 {
        #[inline(always)]
        pub(crate) fn to_array(self) -> [f32; 4] {
            self.0
        }
    }

    macro_rules! binop {
        ($trait:ident, $f:ident, $op:tt, $atrait:ident, $af:ident) => {
            impl $trait for F4 {
                type Output = F4;
                #[inline(always)]
                fn $f(self, o: F4) -> F4 {
                    F4(core::array::from_fn(|i| self.0[i] $op o.0[i]))
                }
            }
            impl $atrait for F4 {
                #[inline(always)]
                fn $af(&mut self, o: F4) {
                    *self = *self $op o;
                }
            }
        };
    }
    binop!(Add, add, +, AddAssign, add_assign);
    binop!(Sub, sub, -, SubAssign, sub_assign);
    impl Mul for F4 {
        type Output = F4;
        #[inline(always)]
        fn mul(self, o: F4) -> F4 {
            F4(core::array::from_fn(|i| self.0[i] * o.0[i]))
        }
    }
    impl Neg for F4 {
        type Output = F4;
        #[inline(always)]
        fn neg(self) -> F4 {
            F4(self.0.map(|x| -x))
        }
    }

    impl super::Lanes for F4 {
        #[inline(always)]
        fn splat(x: f32) -> Self {
            F4([x; 4])
        }
        #[inline(always)]
        fn load(s: &[f32]) -> Self {
            F4(s[..4].try_into().unwrap())
        }
        #[inline(always)]
        fn store(self, s: &mut [f32]) {
            s[..4].copy_from_slice(&self.0);
        }
        #[inline(always)]
        fn gather(s: &[f32], stride: usize) -> Self {
            F4([s[0], s[stride], s[2 * stride], s[3 * stride]])
        }
        #[inline(always)]
        fn scatter(self, s: &mut [f32], stride: usize) {
            s[3 * stride] = self.0[3];
            s[2 * stride] = self.0[2];
            s[stride] = self.0[1];
            s[0] = self.0[0];
        }
        #[inline(always)]
        fn rev(self) -> Self {
            let [a, b, c, d] = self.0;
            F4([d, c, b, a])
        }
    }
}
