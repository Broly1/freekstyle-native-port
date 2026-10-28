//! The VU's 4-lane operations on SSE registers, for compiled microprograms
//! (ee-recomp's VU compiler). Results are bit-identical to the scalar
//! helpers in `vu` and `vumicro` that the interpreter uses; FREEK_VU_CHECK
//! compares the two. SSE2 only, the x86-64 baseline.

use std::arch::x86_64::*;

use crate::vu::V4;

/// One VF register (or ACC): lanes x, y, z, w.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct X(__m128);

// SAFETY (whole module): SSE2 is part of the x86-64 baseline, so these
// intrinsics are always available.

#[inline(always)]
fn ints(x: i32) -> __m128i {
    unsafe { _mm_set1_epi32(x) }
}

/// `m ? a : b`, per bit.
#[inline(always)]
fn sel(m: __m128i, a: __m128i, b: __m128i) -> __m128i {
    unsafe { _mm_or_si128(_mm_and_si128(m, a), _mm_andnot_si128(m, b)) }
}

impl X {
    #[inline(always)]
    pub fn load(v: V4) -> X {
        X(unsafe { _mm_loadu_ps(v.as_ptr()) })
    }

    #[inline(always)]
    pub fn get(self) -> V4 {
        let mut v = [0.0; 4];
        unsafe { _mm_storeu_ps(v.as_mut_ptr(), self.0) };
        v
    }

    #[inline(always)]
    pub fn splat(x: f32) -> X {
        X(unsafe { _mm_set1_ps(x) })
    }

    #[inline(always)]
    pub fn lane(self, n: usize) -> f32 {
        self.get()[n]
    }

    /// Broadcast lane N (the bc field).
    #[inline(always)]
    pub fn bc<const N: usize>(self) -> X {
        let v = self.0;
        X(unsafe {
            match N {
                0 => _mm_shuffle_ps::<0x00>(v, v),
                1 => _mm_shuffle_ps::<0x55>(v, v),
                2 => _mm_shuffle_ps::<0xaa>(v, v),
                _ => _mm_shuffle_ps::<0xff>(v, v),
            }
        })
    }

    #[inline(always)]
    fn bits(self) -> __m128i {
        unsafe { _mm_castps_si128(self.0) }
    }

    #[inline(always)]
    fn from_bits(b: __m128i) -> X {
        X(unsafe { _mm_castsi128_ps(b) })
    }
}

/// `vu::clamp` on each lane: infinities and NaNs become ±MAX, denormals
/// signed zero.
#[inline(always)]
#[cfg(target_feature = "sse4.1")]
fn clamp(x: X) -> X {
    // Same result as the SSE2 version below: |x| capped at FLT_MAX (an
    // integer min also catches inf and NaN), zeroed below the smallest
    // normal, sign put back.
    unsafe {
        let b = x.bits();
        let sign = _mm_and_si128(b, ints(i32::MIN));
        let mag = _mm_min_epi32(_mm_andnot_si128(ints(i32::MIN), b), ints(0x7f7f_ffff));
        let mag = _mm_and_si128(mag, _mm_cmpgt_epi32(mag, ints(0x007f_ffff)));
        X::from_bits(_mm_or_si128(sign, mag))
    }
}

#[inline(always)]
#[cfg(not(target_feature = "sse4.1"))]
fn clamp(x: X) -> X {
    unsafe {
        let b = x.bits();
        let e = _mm_and_si128(b, ints(0x7f80_0000));
        let sign = _mm_and_si128(b, ints(i32::MIN));
        let r = sel(_mm_cmpeq_epi32(e, ints(0)), sign, b);
        X::from_bits(sel(_mm_cmpeq_epi32(e, ints(0x7f80_0000)), _mm_or_si128(sign, ints(0x7f7f_ffff)), r))
    }
}

#[inline(always)]
pub fn add(a: X, b: X) -> X {
    clamp(X(unsafe { _mm_add_ps(a.0, b.0) }))
}

#[inline(always)]
pub fn sub(a: X, b: X) -> X {
    clamp(X(unsafe { _mm_sub_ps(a.0, b.0) }))
}

#[inline(always)]
pub fn mul(a: X, b: X) -> X {
    clamp(X(unsafe { _mm_mul_ps(a.0, b.0) }))
}

#[inline(always)]
pub fn madd(acc: X, a: X, b: X) -> X {
    add(acc, mul(a, b))
}

#[inline(always)]
pub fn msub(acc: X, a: X, b: X) -> X {
    sub(acc, mul(a, b))
}

/// `fpu::fmax`: a > b ? a : b, exactly what MAXPS computes.
#[inline(always)]
pub fn max(a: X, b: X) -> X {
    X(unsafe { _mm_max_ps(a.0, b.0) })
}

/// `fpu::fmin`: a < b ? a : b, exactly what MINPS computes.
#[inline(always)]
pub fn mini(a: X, b: X) -> X {
    X(unsafe { _mm_min_ps(a.0, b.0) })
}

#[inline(always)]
pub fn abs(a: X) -> X {
    X::from_bits(unsafe { _mm_and_si128(a.bits(), ints(i32::MAX)) })
}

/// `vu::cross`: (y*z', z*x', x*y', 0), each clamped.
#[inline(always)]
pub fn cross(a: X, b: X) -> X {
    unsafe {
        // Lanes (x, y, z, w) <- a: (y, z, x, w), b: (z, x, y, w).
        let ra = _mm_shuffle_ps::<0b11_00_10_01>(a.0, a.0);
        let rb = _mm_shuffle_ps::<0b11_01_00_10>(b.0, b.0);
        let p = mul(X(ra), X(rb));
        X::from_bits(_mm_and_si128(p.bits(), _mm_set_epi32(0, -1, -1, -1)))
    }
}

/// `vu::mr32`: (y, z, w, x).
#[inline(always)]
pub fn mr32(a: X) -> X {
    X(unsafe { _mm_shuffle_ps::<0b00_11_10_01>(a.0, a.0) })
}

/// `vu::itof`: fixed point with `frac` fraction bits to float.
#[inline(always)]
pub fn itof(a: X, frac: u32) -> X {
    let scale = 1.0 / (1u32 << frac) as f32;
    X(unsafe { _mm_mul_ps(_mm_cvtepi32_ps(a.bits()), _mm_set1_ps(scale)) })
}

/// `vu::ftoi`: float to fixed point, saturating like Rust's `as i32`
/// (NaN gives 0). CVTTPS2DQ gives 0x80000000 for both overflows and NaN.
#[inline(always)]
pub fn ftoi(a: X, frac: u32) -> X {
    let scale = (1u32 << frac) as f32;
    unsafe {
        let y = _mm_mul_ps(a.0, _mm_set1_ps(scale));
        let i = _mm_cvttps_epi32(y);
        let over = _mm_castps_si128(_mm_cmpge_ps(y, _mm_set1_ps(2_147_483_648.0)));
        let nan = _mm_castps_si128(_mm_cmpunord_ps(y, y));
        let i = sel(over, ints(i32::MAX), i);
        X::from_bits(_mm_andnot_si128(nan, i))
    }
}

/// Lanes of `new` where `DEST` has a bit (bit 3 = x), else `old`.
#[inline(always)]
pub fn blend<const DEST: u8>(old: X, new: X) -> X {
    let on = |bit: u8| if DEST & bit != 0 { -1 } else { 0 };
    let m = unsafe { _mm_set_epi32(on(1), on(2), on(4), on(8)) };
    X::from_bits(sel(m, new.bits(), old.bits()))
}

/// Lane mask bits (x in bit 0) to VU field order (x in bit 3).
const REV: [u32; 16] = [0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15];

/// `vumicro::mac_flags`: Z, S and O bits of the fields in `dest`.
#[inline(always)]
pub fn mac_flags(v: X, dest: u8) -> u32 {
    unsafe {
        let b = v.bits();
        let mag = _mm_and_si128(b, ints(i32::MAX));
        let z = _mm_cmpeq_epi32(mag, ints(0));
        let s = _mm_andnot_si128(z, _mm_cmplt_epi32(b, ints(0)));
        let o = _mm_cmpeq_epi32(mag, ints(0x7f7f_ffff));
        let m = |x: __m128i| REV[_mm_movemask_ps(_mm_castsi128_ps(x)) as usize] & dest as u32;
        m(z) | m(s) << 4 | m(o) << 12
    }
}

/// `vumicro::clip_judge`: x, y, z of `s` against ±|t.w|.
#[inline(always)]
pub fn clip_judge(s: X, t: X) -> u32 {
    unsafe {
        let w = abs(t).bc::<3>();
        let nw = X::from_bits(_mm_xor_si128(w.bits(), ints(i32::MIN)));
        let gt = _mm_movemask_ps(_mm_cmpgt_ps(s.0, w.0)) as u32;
        let lt = _mm_movemask_ps(_mm_cmplt_ps(s.0, nw.0)) as u32;
        let spread = |m: u32| (m & 1) | (m & 2) << 1 | (m & 4) << 2;
        spread(gt) | spread(lt) << 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vu;

    /// Awkward values: zeros, denormals, MAX, infinities, NaNs, large
    /// and small normals of both signs.
    fn samples() -> Vec<f32> {
        let bits = [
            0u32, 0x8000_0000, 1, 0x8000_0001, 0x007f_ffff, 0x0080_0000, 0x3f80_0000, 0xbf80_0000, 0x7f7f_ffff, 0xff7f_ffff, 0x7f80_0000, 0xff80_0000,
            0x7fc0_0000, 0xffc0_0001, 0x4f00_0000, 0xcf00_0001, 0x4effffff, 0x3e99_999a, 0x7e80_0000, 0x0100_0000, 0x4b00_0001,
        ];
        bits.iter().map(|&b| f32::from_bits(b)).collect()
    }

    fn same(a: V4, b: V4) -> bool {
        a.map(f32::to_bits) == b.map(f32::to_bits)
    }

    /// Like `same`, but ignoring lanes where both inputs are NaN: which one
    /// an IEEE operation returns is unspecified, so the scalar helpers may
    /// differ from build to build there too.
    fn same2(x: V4, y: V4, a: V4, b: V4) -> bool {
        (0..4).all(|k| (a[k].is_nan() && b[k].is_nan()) || x[k].to_bits() == y[k].to_bits())
    }

    /// The vector clamp agrees with the scalar one for every sign and
    /// exponent, with mantissas at and between the edges.
    #[test]
    fn clamp_matches_scalar() {
        for se in 0..512u32 {
            for m in [0, 1, 2, 0x1234, 0x3f_ffff, 0x40_0000, 0x40_0001, 0x7f_fffe, 0x7f_ffff] {
                let x = f32::from_bits(se << 23 | m);
                let want = vu::clamp(x).to_bits();
                assert_eq!(clamp(X::splat(x)).get()[0].to_bits(), want, "{:#010x}", x.to_bits());
            }
        }
    }

    #[test]
    fn matches_scalar() {
        let s = samples();
        let n = s.len();
        for i in 0..n * n {
            let a: V4 = std::array::from_fn(|k| s[(i + k * 7) % n]);
            let b: V4 = std::array::from_fn(|k| s[(i / n + k * 3) % n]);
            let c: V4 = std::array::from_fn(|k| s[(i * 5 + k) % n]);
            let (xa, xb, xc) = (X::load(a), X::load(b), X::load(c));
            assert!(same2(add(xa, xb).get(), vu::add(a, b), a, b));
            assert!(same2(sub(xa, xb).get(), vu::sub(a, b), a, b));
            assert!(same2(mul(xa, xb).get(), vu::mul(a, b), a, b));
            assert!(same2(madd(xc, xa, xb).get(), vu::madd(c, a, b), a, b));
            assert!(same2(msub(xc, xa, xb).get(), vu::msub(c, a, b), a, b));
            assert!(same(max(xa, xb).get(), vu::max(a, b)));
            assert!(same(mini(xa, xb).get(), vu::mini(a, b)));
            assert!(same(abs(xa).get(), vu::abs(a)));
            assert!(same(cross(xa, xb).get(), vu::cross(a, b)));
            assert!(same(mr32(xa).get(), vu::mr32(a)));
            for f in [0, 4, 12, 15] {
                assert!(same(itof(xa, f).get(), vu::itof(a, f)));
                assert!(same(ftoi(xa, f).get(), vu::ftoi(a, f)), "ftoi {f} {a:?}");
            }
            assert!(same(xb.bc::<2>().get(), [b[2]; 4]));
            assert!(same(blend::<0b1010>(xa, xb).get(), [b[0], a[1], b[2], a[3]]));
            for dest in [0xf, 0x8, 0x5, 0x2] {
                assert_eq!(mac_flags(xa, dest), crate::vumicro::mac_flags(a, dest));
            }
            assert_eq!(clip_judge(xa, xb), crate::vumicro::clip_judge(a, b));
        }
    }
}
