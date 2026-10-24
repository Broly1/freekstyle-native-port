//! MMI: the EE's 128-bit integer SIMD instructions, on u128 registers.
//! Lane 0 is the least significant.

macro_rules! lanes {
    ($name:ident, $t:ty, $n:expr) => {
        #[inline(always)]
        fn $name(v: u128) -> [$t; $n] {
            unsafe { std::mem::transmute::<u128, [$t; $n]>(v) }
        }
    };
}
#[inline(always)]
fn b(v: u128) -> [u8; 16] {
    v.to_le_bytes()
}
lanes!(h, u16, 8);
lanes!(w, u32, 4);
lanes!(d, u64, 2);

#[inline(always)]
fn pack<T: Copy, const N: usize>(v: [T; N]) -> u128 {
    assert!(std::mem::size_of::<[T; N]>() == 16);
    unsafe { std::mem::transmute_copy::<[T; N], u128>(&v) }
}

#[inline(always)]
pub fn paddub(s: u128, t: u128) -> u128 {
    let (s, t) = (b(s), b(t));
    pack(std::array::from_fn::<u8, 16, _>(|i| s[i].saturating_add(t[i])))
}

#[inline(always)]
pub fn psubb(s: u128, t: u128) -> u128 {
    let (s, t) = (b(s), b(t));
    pack(std::array::from_fn::<u8, 16, _>(|i| s[i].wrapping_sub(t[i])))
}

#[inline(always)]
pub fn paddh(s: u128, t: u128) -> u128 {
    let (s, t) = (h(s), h(t));
    pack(std::array::from_fn::<u16, 8, _>(|i| s[i].wrapping_add(t[i])))
}

#[inline(always)]
pub fn paddw(s: u128, t: u128) -> u128 {
    let (s, t) = (w(s), w(t));
    pack(std::array::from_fn::<u32, 4, _>(|i| s[i].wrapping_add(t[i])))
}

#[inline(always)]
pub fn psubw(s: u128, t: u128) -> u128 {
    let (s, t) = (w(s), w(t));
    pack(std::array::from_fn::<u32, 4, _>(|i| s[i].wrapping_sub(t[i])))
}

#[inline(always)]
pub fn pcgtw(s: u128, t: u128) -> u128 {
    let (s, t) = (w(s), w(t));
    pack(std::array::from_fn::<u32, 4, _>(|i| if (s[i] as i32) > (t[i] as i32) { !0 } else { 0 }))
}

#[inline(always)]
pub fn pmaxh(s: u128, t: u128) -> u128 {
    let (s, t) = (h(s), h(t));
    pack(std::array::from_fn::<u16, 8, _>(|i| (s[i] as i16).max(t[i] as i16) as u16))
}

#[inline(always)]
pub fn pminh(s: u128, t: u128) -> u128 {
    let (s, t) = (h(s), h(t));
    pack(std::array::from_fn::<u16, 8, _>(|i| (s[i] as i16).min(t[i] as i16) as u16))
}

#[inline(always)]
pub fn pand(s: u128, t: u128) -> u128 {
    s & t
}

#[inline(always)]
pub fn por(s: u128, t: u128) -> u128 {
    s | t
}

#[inline(always)]
pub fn pxor(s: u128, t: u128) -> u128 {
    s ^ t
}

#[inline(always)]
pub fn pnor(s: u128, t: u128) -> u128 {
    !(s | t)
}

/// Interleave the low halves: rt lane 0, rs lane 0, rt lane 1, ...
#[inline(always)]
pub fn pextlw(s: u128, t: u128) -> u128 {
    let (s, t) = (w(s), w(t));
    pack([t[0], s[0], t[1], s[1]])
}

#[inline(always)]
pub fn pextlh(s: u128, t: u128) -> u128 {
    let (s, t) = (h(s), h(t));
    pack(std::array::from_fn::<u16, 8, _>(|i| if i % 2 == 0 { t[i / 2] } else { s[i / 2] }))
}

#[inline(always)]
pub fn pextlb(s: u128, t: u128) -> u128 {
    let (s, t) = (b(s), b(t));
    pack(std::array::from_fn::<u8, 16, _>(|i| if i % 2 == 0 { t[i / 2] } else { s[i / 2] }))
}

/// Pack the even bytes: low half from rt, high half from rs.
#[inline(always)]
pub fn ppacb(s: u128, t: u128) -> u128 {
    let (s, t) = (b(s), b(t));
    pack(std::array::from_fn::<u8, 16, _>(|i| if i < 8 { t[i * 2] } else { s[(i - 8) * 2] }))
}

#[inline(always)]
pub fn ppach(s: u128, t: u128) -> u128 {
    let (s, t) = (h(s), h(t));
    pack(std::array::from_fn::<u16, 8, _>(|i| if i < 4 { t[i * 2] } else { s[(i - 4) * 2] }))
}

#[inline(always)]
pub fn pcpyld(s: u128, t: u128) -> u128 {
    pack([d(t)[0], d(s)[0]])
}

#[inline(always)]
pub fn pcpyud(s: u128, t: u128) -> u128 {
    pack([d(s)[1], d(t)[1]])
}

#[inline(always)]
pub fn pcpyh(t: u128) -> u128 {
    let t = h(t);
    pack([t[0], t[0], t[0], t[0], t[4], t[4], t[4], t[4]])
}

#[inline(always)]
pub fn pexch(t: u128) -> u128 {
    let t = h(t);
    pack([t[0], t[2], t[1], t[3], t[4], t[6], t[5], t[7]])
}

#[inline(always)]
pub fn pexcw(t: u128) -> u128 {
    let t = w(t);
    pack([t[0], t[2], t[1], t[3]])
}

#[inline(always)]
pub fn psllw(t: u128, sa: u32) -> u128 {
    pack(w(t).map(|x| x << sa))
}

#[inline(always)]
pub fn psrlw(t: u128, sa: u32) -> u128 {
    pack(w(t).map(|x| x >> sa))
}

#[inline(always)]
pub fn psraw(t: u128, sa: u32) -> u128 {
    pack(w(t).map(|x| ((x as i32) >> sa) as u32))
}

/// Leading sign-bit count minus one, for both low words. 64-bit result.
#[inline(always)]
pub fn plzcw(s: u64) -> u64 {
    let f = |x: u32| {
        let n = if (x as i32) < 0 { (!x).leading_zeros() } else { x.leading_zeros() };
        (n - 1) as u64
    };
    f(s as u32) | f((s >> 32) as u32) << 32
}

#[inline(always)]
pub fn psllh(t: u128, sa: u32) -> u128 {
    pack(h(t).map(|x| x << (sa & 15)))
}

#[inline(always)]
pub fn psrlh(t: u128, sa: u32) -> u128 {
    pack(h(t).map(|x| x >> (sa & 15)))
}

#[inline(always)]
pub fn psrah(t: u128, sa: u32) -> u128 {
    pack(h(t).map(|x| ((x as i16) >> (sa & 15)) as u16))
}

#[inline(always)]
pub fn pcgth(s: u128, t: u128) -> u128 {
    let (s, t) = (h(s), h(t));
    pack(std::array::from_fn::<u16, 8, _>(|i| if (s[i] as i16) > (t[i] as i16) { !0 } else { 0 }))
}

/// Interleave the high halves' bytes: rt byte 8, rs byte 8, rt byte 9, ...
#[inline(always)]
pub fn pextub(s: u128, t: u128) -> u128 {
    let (s, t) = (b(s), b(t));
    pack(std::array::from_fn::<u8, 16, _>(|i| if i % 2 == 0 { t[8 + i / 2] } else { s[8 + i / 2] }))
}

/// Funnel shift right of rs:rt by the SA register (in bits).
#[inline(always)]
pub fn qfsrv(s: u128, t: u128, sa: u32) -> u128 {
    let sa = sa & 127;
    if sa == 0 { t } else { (t >> sa) | (s << (128 - sa)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_and_copy() {
        let s = 0x0000000f_0000000e_0000000d_0000000c_u128;
        let t = 0x0000000b_0000000a_00000009_00000008_u128;
        assert_eq!(pextlw(s, t), 0x0000000d_00000009_0000000c_00000008);
        assert_eq!(pcpyld(s, t), ((s as u64 as u128) << 64) | (t as u64 as u128));
        assert_eq!(pcpyud(s, t), (t >> 64 << 64) | (s >> 64));
        assert_eq!(paddub(0xff01, 0x0101), 0xff02);
        assert_eq!(plzcw(0xffff_ffff_0000_0001), (31 << 32) | 30);
        assert_eq!(qfsrv(0x11, 0x22, 8), (0x11 << 120) | 0);
        assert_eq!(psrah(0x8000, 15), 0xffff);
    }
}
