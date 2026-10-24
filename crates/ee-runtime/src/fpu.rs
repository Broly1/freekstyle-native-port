//! PS2 floating point. The EE FPU and the VUs are not IEEE 754: there are
//! no infinities, NaNs or denormals. Overflow saturates to ±MAX and
//! denormals flush to zero. Host IEEE math followed by [`ps2f`] matches that
//! closely enough for game logic. Exact rounding differences are left for
//! later.

#[inline(always)]
pub fn ps2f(x: f32) -> f32 {
    if x.is_finite() && !x.is_subnormal() {
        return x;
    }
    clamp_slow(x)
}

#[cold]
fn clamp_slow(x: f32) -> f32 {
    if x.is_subnormal() {
        0.0f32.copysign(x)
    } else {
        f32::MAX.copysign(x)
    }
}

/// Division: divide-by-zero yields ±MAX with the XOR of the signs.
#[inline(always)]
pub fn fdiv(a: f32, b: f32) -> f32 {
    if b == 0.0 {
        let neg = a.is_sign_negative() != b.is_sign_negative();
        return if neg { -f32::MAX } else { f32::MAX };
    }
    ps2f(a / b)
}

#[inline(always)]
pub fn fsqrt(a: f32) -> f32 {
    a.abs().sqrt()
}

/// cvt.w.s: truncate toward zero, saturating. Rust's `as` already does both.
#[inline(always)]
pub fn cvt_w(a: f32) -> u32 {
    a as i32 as u32
}

#[inline(always)]
pub fn fmax(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

#[inline(always)]
pub fn fmin(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}
