//! GS alpha blending, `((A - B) * C >> 7) + D`, mapped to Vulkan blend
//! state.
//!
//! A, B and D select Cs (source), Cd (destination) or 0; C selects As, Ad or
//! FIX. Render targets store alpha as a/128, so As, Ad and FIX/128 all map
//! to [0, 1] factors (SRC_ALPHA, DST_ALPHA, CONSTANT_ALPHA). Expanding the
//! formula gives a source and a destination coefficient each drawn from
//! {0, 1, C, 1-C, -C, 1+C}; every combination fits one fixed-function
//! equation except a (1+C) source term, which the shader pre-scales.

use ash::vk;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Blend {
    pub enable: bool,
    pub op: vk::BlendOp,
    pub src: vk::BlendFactor,
    pub dst: vk::BlendFactor,
    /// Shader source scale: 0 none, 1 (1+As), 2 (1+FIX).
    pub shader_scale: u32,
    /// The equation could only be approximated.
    pub approx: bool,
}

impl Blend {
    pub const OFF: Blend = Blend {
        enable: false,
        op: vk::BlendOp::ADD,
        src: vk::BlendFactor::ONE,
        dst: vk::BlendFactor::ZERO,
        shader_scale: 0,
        approx: false,
    };
}

/// Coefficient of one colour in the expanded formula.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coef {
    Zero,
    One,
    C,
    OneMinusC,
    NegC,
    OnePlusC,
}

fn coef(a: bool, b: bool, d: bool) -> Coef {
    match (a, b, d) {
        (true, true, true) | (false, false, true) => Coef::One,
        (true, true, false) | (false, false, false) => Coef::Zero,
        (true, false, false) => Coef::C,
        (true, false, true) => Coef::OnePlusC,
        (false, true, false) => Coef::NegC,
        (false, true, true) => Coef::OneMinusC,
    }
}

pub fn map(abcd: (u32, u32, u32, u32)) -> Blend {
    let (a, b, c, d) = abcd;
    const CS: u32 = 0;
    const CD: u32 = 1;
    let src = coef(a == CS, b == CS, d == CS);
    let dst = coef(a == CD, b == CD, d == CD);
    let (fc, fc1) = match c {
        0 => (vk::BlendFactor::SRC_ALPHA, vk::BlendFactor::ONE_MINUS_SRC_ALPHA),
        1 => (vk::BlendFactor::DST_ALPHA, vk::BlendFactor::ONE_MINUS_DST_ALPHA),
        _ => (vk::BlendFactor::CONSTANT_ALPHA, vk::BlendFactor::ONE_MINUS_CONSTANT_ALPHA),
    };
    let factor = |k: Coef| match k {
        Coef::Zero => vk::BlendFactor::ZERO,
        Coef::One | Coef::OnePlusC => vk::BlendFactor::ONE,
        Coef::C | Coef::NegC => fc,
        Coef::OneMinusC => fc1,
    };
    let neg = |k: Coef| k == Coef::NegC;
    let op = match (neg(src), neg(dst)) {
        (false, false) => vk::BlendOp::ADD,
        (false, true) => vk::BlendOp::SUBTRACT,
        (true, false) => vk::BlendOp::REVERSE_SUBTRACT,
        // -C*Cs - C*Cd: always <= 0, clamps to black.
        (true, true) => {
            return Blend { enable: true, op: vk::BlendOp::ADD, src: vk::BlendFactor::ZERO, dst: vk::BlendFactor::ZERO, shader_scale: 0, approx: false }
        }
    };
    // A (1+C) source term is applied in the shader when C does not depend on
    // the destination.
    let (shader_scale, approx) = match (src, c) {
        (Coef::OnePlusC, 0) => (1, false),
        (Coef::OnePlusC, 2) => (2, false),
        (Coef::OnePlusC, _) => (0, true),
        _ => (0, dst == Coef::OnePlusC),
    };
    let blend = Blend { enable: true, op, src: factor(src), dst: factor(dst), shader_scale, approx };
    // Pure copies need no blending.
    if blend.src == vk::BlendFactor::ONE && blend.dst == vk::BlendFactor::ZERO && op == vk::BlendOp::ADD && shader_scale == 0 {
        return Blend { approx, ..Blend::OFF };
    }
    blend
}

#[cfg(test)]
mod tests {
    use super::*;
    use vk::BlendFactor as F;

    #[test]
    fn common_modes() {
        // (Cs - Cd) * As + Cd: standard alpha blending.
        let b = map((0, 1, 0, 1));
        assert_eq!((b.op, b.src, b.dst), (vk::BlendOp::ADD, F::SRC_ALPHA, F::ONE_MINUS_SRC_ALPHA));
        // (Cs - 0) * As + Cd: additive.
        let b = map((0, 2, 0, 1));
        assert_eq!((b.op, b.src, b.dst), (vk::BlendOp::ADD, F::SRC_ALPHA, F::ONE));
        // (0 - Cs) * FIX + Cd: subtractive with a constant.
        let b = map((2, 0, 2, 1));
        assert_eq!((b.op, b.src, b.dst), (vk::BlendOp::REVERSE_SUBTRACT, F::CONSTANT_ALPHA, F::ONE));
        // (Cd - Cs) * Ad + Cs.
        let b = map((1, 0, 1, 0));
        assert_eq!((b.op, b.src, b.dst), (vk::BlendOp::ADD, F::ONE_MINUS_DST_ALPHA, F::DST_ALPHA));
        // (Cs - Cs) * x + Cs: plain write.
        assert!(!map((0, 0, 0, 0)).enable);
        // (Cs - 0) * As + Cs: (1+As) * Cs via the shader.
        let b = map((0, 2, 0, 0));
        assert_eq!((b.src, b.dst, b.shader_scale), (F::ONE, F::ZERO, 1));
    }

    #[test]
    fn every_combination_maps() {
        let mut exact = 0;
        for a in 0..3 {
            for b in 0..3 {
                for c in 0..3 {
                    for d in 0..3 {
                        exact += u32::from(!map((a, b, c, d)).approx);
                    }
                }
            }
        }
        // Approximated: (1+C)*Cd (A=D=Cd, B in {Cs,0}, any C: 6 cases) and
        // (1+Ad)*Cs (A=D=Cs, B in {Cd,0}, C=Ad: 2 cases).
        assert_eq!(exact, 81 - 8);
    }
}
