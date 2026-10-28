//! Vector unit micro mode: an interpreter for VU0/VU1 microprograms.
//!
//! Each 64-bit instruction is a pair: the upper word (FMAC, the same
//! encoding as COP2 macro instructions) and the lower word (loads/stores,
//! integer ops, branches, divider, EFU, XGKICK). Behaviour games rely on:
//!
//! - Both halves read registers before either writes. If both write the
//!   same VF register, the upper result wins.
//! - Upper bit 31 (I): the lower word is a float loaded into I, visible
//!   from the next pair. Bit 30 (E): the program ends after the next pair.
//! - Branches and jumps have one delay slot. A conditional branch that
//!   reads a VI register written by the immediately preceding pair sees the
//!   value from before that write.
//! - DIV/SQRT give Q after 7 cycles and RSQRT after 13, so Q read earlier is
//!   the previous value. FMAC MAC/status flags and CLIP flags become visible
//!   4 cycles after the instruction. One pair is one cycle; the stalls of
//!   real hardware only lengthen this, which correct programs tolerate.


use r5900::{decode, Mnemonic as M};

use crate::fpu::{fdiv, fsqrt, ps2f};
use crate::vu::{self, VuRegs, V4};

/// Where a microprogram runs.
pub struct Env<'a> {
    pub code: &'a [u8],
    pub data: &'a mut [u8],
    /// VIF1 TOP and ITOP, read by XTOP/XITOP.
    pub top: u32,
    pub itop: u32,
    /// XGKICKed GIF packets are appended here (VU1).
    pub kick: Option<&'a mut Vec<u128>>,
}

/// Decoded upper instructions, filled lazily and dropped when the
/// program memory changes (`Memory::vu_code_gen`).
#[derive(Default)]
pub struct DecodeCache {
    gen: u64,
    upper: Vec<Option<M>>,
    /// FNV-1a of the program image, identifying recompiled code.
    hash: Option<u64>,
    /// Whether the image reads MAC/status flags (see `reads_flags`).
    flags: Option<bool>,
}

/// Whether any instruction in a program image reads the MAC or status
/// flags (FSAND/FSEQ/FSOR/FSSET, FMAND/FMEQ/FMOR). Divider status bits are
/// still kept either way.
pub fn reads_flags(code: &[u8]) -> bool {
    code.chunks_exact(8).any(|p| {
        let lo = u32::from_le_bytes(p[0..4].try_into().unwrap());
        let hi = u32::from_le_bytes(p[4..8].try_into().unwrap());
        hi >> 31 == 0 && matches!(lo >> 25, 0x14..=0x18 | 0x1a | 0x1b)
    })
}

impl DecodeCache {
    /// Make the cache match code generation `gen` for `n` instructions.
    pub fn sync(&mut self, gen: u64, n: usize) {
        if self.gen != gen || self.upper.len() != n {
            self.gen = gen;
            self.upper.clear();
            self.upper.resize(n, None);
            self.hash = None;
            self.flags = None;
        }
    }
}

/// A recompiled microprogram, entered at a fixed instruction.
pub type NativeFn = fn(&mut Ctx);

/// Recompiled programs: (unit, image hash, entry instruction, function),
/// sorted. Installed by the frontend from generated code.
pub struct Programs(pub &'static [(u8, u64, u32, NativeFn)]);

impl Programs {
    fn find(&self, unit: u8, hash: u64, entry: u32) -> Option<NativeFn> {
        self.0.binary_search_by_key(&(unit, hash, entry), |e| (e.0, e.1, e.2)).ok().map(|i| self.0[i].3)
    }
}

/// (unit, hash, entry) already reported to the dump directory.
static RECORDED: std::sync::Mutex<Option<std::collections::HashSet<(u8, u64, u32)>>> = std::sync::Mutex::new(None);

/// FREEK_VU_DUMP=<dir>: save each interpreted program image and its entry
/// points for ee-recomp to compile.
fn record(unit: u8, hash: u64, entry: u32, code: &[u8]) {
    let Some(dir) = std::env::var_os("FREEK_VU_DUMP") else { return };
    let mut seen = RECORDED.lock().unwrap();
    if !seen.get_or_insert_with(Default::default).insert((unit, hash, entry)) {
        return;
    }
    let dir = std::path::PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    let base = dir.join(format!("vu{unit}_{hash:016x}"));
    let image = base.with_extension("bin");
    if !image.exists() {
        let _ = std::fs::write(&image, code);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(base.with_extension("entries")) {
        let _ = writeln!(f, "{entry:04x}");
    }
}

/// FREEK_VU_DUMP: save a JR/JALR target compiled code did not know, as
/// "site target" lines in <image>.jumps.
fn record_jump((unit, hash): (u8, u64), site: u32, target: u32) {
    let Some(dir) = std::env::var_os("FREEK_VU_DUMP") else { return };
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<(u8, u64, u32, u32)>>> = std::sync::Mutex::new(None);
    if !SEEN.lock().unwrap().get_or_insert_with(Default::default).insert((unit, hash, site, target)) {
        return;
    }
    let dir = std::path::PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("vu{unit}_{hash:016x}.jumps"));
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{site:04x} {target:04x}");
    }
}

pub fn image_hash(code: &[u8]) -> u64 {
    code.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// Runaway guard: no real microprogram runs this long.
const MAX_CYCLES: u64 = 1 << 24;
const FLAG_LATENCY: u64 = 4;
/// FMAC results are readable this many cycles after issue.
const FMAC_LATENCY: u64 = 4;

/// VF register fields a pair reads, as (register, field mask) with bit 3 =
/// x .. bit 0 = w. Used to model the hazard stalls programs are timed for.
#[inline(always)]
fn reads(lo: u32, hi: u32, m: M) -> impl Iterator<Item = (usize, u8)> {
    use r5900::Format as F;
    let dest = ((hi >> 21) & 15) as u8;
    let (fs, ft) = (((hi >> 11) & 31) as usize, ((hi >> 16) & 31) as usize);
    let bc = 8u8 >> (hi & 3);
    let mut out = [(0usize, 0u8); 4];
    let mut n = 0;
    let mut push = |reg: usize, mask: u8| {
        if reg != 0 && mask != 0 {
            out[n] = (reg, mask);
            n += 1;
        }
    };
    match m.format() {
        F::VFdFsFt | F::VAccFsFt => {
            push(fs, dest);
            push(ft, dest);
        }
        F::VFdFsFtBc | F::VAccFsFtBc => {
            push(fs, dest);
            push(ft, bc);
        }
        F::VFdFsQ | F::VFdFsI | F::VAccFsQ | F::VAccFsI | F::VFtFs => push(fs, dest),
        F::VClip => {
            push(fs, 0xe);
            push(ft, 0x1);
        }
        _ => {}
    }
    if hi >> 31 == 0 {
        let (lfs, lft) = (((lo >> 11) & 31) as usize, ((lo >> 16) & 31) as usize);
        let ldest = ((lo >> 21) & 15) as u8;
        let (fsf, ftf) = (8u8 >> ((lo >> 21) & 3), 8u8 >> ((lo >> 23) & 3));
        match lo >> 25 {
            0x01 => push(lfs, ldest), // SQ
            0x40 if lo & 63 >= 0x3c => match ((lo >> 6) & 31) << 2 | (lo & 3) {
                0x30 | 0x35 | 0x37 => push(lfs, ldest), // MOVE, SQI, SQD
                0x31 => push(lfs, 0xf),                 // MR32
                0x38 | 0x3a => {
                    push(lfs, fsf);
                    push(lft, ftf);
                }
                0x39 => push(lft, ftf),
                0x3c | 0x42 | 0x43 | 0x78..=0x7e => push(lfs, fsf),
                0x70..=0x76 => push(lfs, 0xf),
                _ => {}
            },
            _ => {}
        }
    }
    out.into_iter().take(n)
}

/// Pending writes to one flag register: at most one per cycle, each
/// visible FLAG_LATENCY cycles later, so a short ring suffices.
#[derive(Default)]
struct Ring {
    buf: [(u64, u32); 8],
    head: usize,
    len: usize,
}

impl Ring {
    /// Queue a write. Rings are only drained when read, so when full the
    /// oldest entry (long since due, at one write per cycle) is applied.
    fn push(&mut self, ready: u64, v: u32, reg: &mut u32) {
        if self.len == self.buf.len() {
            *reg = self.buf[self.head].1;
            self.head = (self.head + 1) % self.buf.len();
            self.len -= 1;
        }
        self.buf[(self.head + self.len) % self.buf.len()] = (ready, v);
        self.len += 1;
    }

    /// Apply writes that have become visible by `cycle`.
    fn advance(&mut self, cycle: u64, reg: &mut u32) {
        while self.len > 0 && self.buf[self.head].0 <= cycle {
            *reg = self.buf[self.head].1;
            self.head = (self.head + 1) % self.buf.len();
            self.len -= 1;
        }
    }

    /// The newest value, pending or not.
    fn latest(&self, reg: u32) -> u32 {
        if self.len == 0 {
            reg
        } else {
            self.buf[(self.head + self.len - 1) % self.buf.len()].1
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

#[derive(Default)]
struct Pipe {
    /// Pending Q: (ready cycle, value).
    q: Option<(u64, f32)>,
    mac: Ring,
    status: Ring,
    clip: Ring,
}

impl Pipe {
    /// Make Q current for `cycle` (cheap; done every pair).
    #[inline(always)]
    fn advance_q(&mut self, r: &mut VuRegs, cycle: u64) {
        if let Some((ready, v)) = self.q {
            if ready <= cycle {
                r.q = v;
                self.q = None;
            }
        }
    }

    /// Make Q and the flags current for `cycle` (before reading flags).
    fn advance(&mut self, r: &mut VuRegs, cycle: u64) {
        self.advance_q(r, cycle);
        self.mac.advance(cycle, &mut r.mac);
        self.status.advance(cycle, &mut r.status);
        self.clip.advance(cycle, &mut r.clip);
    }

    fn flush(&mut self, r: &mut VuRegs) {
        self.advance(r, u64::MAX);
    }
}

fn field(sel: u32) -> usize {
    sel as usize & 3
}

/// First field set in a destination mask (x first).
fn dest_field(dest: u8) -> usize {
    (0..4).find(|n| dest & (8 >> n) != 0).unwrap_or(0)
}

fn sext11(v: u32) -> i32 {
    ((v & 0x7ff) as i32) << 21 >> 21
}

pub struct Mem<'a> {
    data: &'a mut [u8],
}

impl Mem<'_> {
    #[inline(always)]
    fn qwords(&self) -> usize {
        self.data.len() / 16
    }
    #[inline(always)]
    fn word(&self, q: usize, n: usize) -> u32 {
        let a = (q & (self.qwords() - 1)) * 16 + n * 4;
        u32::from_le_bytes(self.data[a..a + 4].try_into().unwrap())
    }
    #[inline(always)]
    fn set_word(&mut self, q: usize, n: usize, v: u32) {
        let a = (q & (self.qwords() - 1)) * 16 + n * 4;
        self.data[a..a + 4].copy_from_slice(&v.to_le_bytes());
    }
    #[inline(always)]
    fn vec(&self, q: usize) -> V4 {
        std::array::from_fn(|n| f32::from_bits(self.word(q, n)))
    }
    #[inline(always)]
    fn store(&mut self, q: usize, dest: u8, v: V4) {
        for (n, x) in v.iter().enumerate() {
            if dest & (8 >> n) != 0 {
                self.set_word(q, n, x.to_bits());
            }
        }
    }
    #[inline(always)]
    fn qword(&self, q: usize) -> u128 {
        (0..4).fold(0u128, |a, n| a | (self.word(q, n) as u128) << (32 * n))
    }
    /// Append `n` qwords from `q` on, wrapping at the end of memory.
    fn extend_qwords(&self, mut q: usize, mut n: usize, out: &mut Vec<u128>) {
        out.reserve(n);
        while n > 0 {
            let at = q & (self.qwords() - 1);
            let run = n.min(self.qwords() - at);
            out.extend(self.data[at * 16..(at + run) * 16].chunks_exact(16).map(|c| u128::from_le_bytes(c.try_into().unwrap())));
            (q, n) = (q + run, n - run);
        }
    }
}

/// MAC and status flags for an FMAC result in the given fields.
/// Branch-free: this runs for every FMAC instruction.
#[inline(always)]
fn fmac_flags(v: V4, dest: u8, old_status: u32) -> (u32, u32) {
    let mut mac = 0u32;
    for (n, x) in v.iter().enumerate() {
        let b = x.to_bits();
        let mag = b & 0x7fff_ffff;
        let on = u32::from(dest >> (3 - n) & 1);
        let bit = 3 - n as u32;
        let z = u32::from(mag == 0);
        let s = (b >> 31) & (1 - z);
        let o = u32::from(mag == 0x7f7f_ffff);
        mac |= on * (z << bit | s << (bit + 4) | o << (bit + 12));
    }
    let zsuo = u32::from(mac & 0xf != 0) | u32::from(mac & 0xf0 != 0) << 1 | u32::from(mac & 0xf000 != 0) << 3;
    // Keep the divider's I/D bits; sticky bits accumulate.
    let status = (old_status & 0x30) | zsuo | (old_status & 0xfc0) | zsuo << 6;
    (mac, status)
}

/// Microprogram statistics for FREEK_VU_LOG: (runs, cycles, nanoseconds).
pub static STATS: std::sync::Mutex<(u64, u64, u64)> = std::sync::Mutex::new((0, 0, 0));
/// Runs that used recompiled code.
pub static NATIVE_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Run a microprogram from instruction `start` until its E bit. Returns
/// the cycles executed.
pub fn run<'a>(r: &'a mut VuRegs, env: Env<'a>, cache: &mut DecodeCache, unit: u8, programs: &Programs, start: u32) -> u64 {
    let t0 = crate::stats_enabled().then(std::time::Instant::now);
    let n = env.code.len() / 8;
    if cache.upper.len() != n {
        cache.sync(cache.gen, n);
    }
    let start = start & (n as u32 - 1);
    let hash = *cache.hash.get_or_insert_with(|| image_hash(env.code));
    let native = programs.find(unit, hash, start);
    static TOP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *TOP.get_or_init(|| std::env::var_os("FREEK_VU_TOP").is_some()) {
        vu_top(unit, hash, start);
    }
    if native.is_none() {
        record(unit, hash, start, env.code);
        if std::env::var_os("FREEK_VU_MISS").is_some() {
            static MISSES: std::sync::Mutex<Option<std::collections::HashSet<(u8, u64, u32)>>> = std::sync::Mutex::new(None);
            if MISSES.lock().unwrap().get_or_insert_with(Default::default).insert((unit, hash, start)) {
                eprintln!("[vu] no native code for vu{unit} {hash:016x} entry {start:#x}");
            }
        }
    }
    if let (Some(f), true) = (native, check_enabled()) {
        return check(f, r, env, cache, unit, hash, start);
    }
    let mut c = Ctx::new(r, env);
    c.id = (unit, hash);
    match native {
        Some(f) => {
            NATIVE_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            f(&mut c)
        }
        None => interpret(&mut c, cache, start),
    }
    let cycles = c.cycle;
    if let Some(t0) = t0 {
        let mut s = STATS.lock().unwrap();
        *s = (s.0 + 1, s.1 + cycles, s.2 + t0.elapsed().as_nanos() as u64);
    }
    cycles
}

/// State of one microprogram run, shared by the interpreter and
/// recompiled programs.
pub struct Ctx<'a> {
    pub r: &'a mut VuRegs,
    code: &'a [u8],
    mem: Mem<'a>,
    top: u32,
    itop: u32,
    kick: Option<&'a mut Vec<u128>>,
    pipe: Pipe,
    pub cycle: u64,
    /// Cycle at which each VF register field's pending FMAC result is
    /// available ([reg][x, y, z, w]); reading earlier stalls.
    ready: [[u64; 4]; 32],
    /// VI register written by the previous pair, with its old value.
    last_vi: Option<(usize, u16)>,
    mask: u32,
    /// (unit, image hash), for recording jump targets.
    id: (u8, u64),
}

impl<'a> Ctx<'a> {
    pub fn new(r: &'a mut VuRegs, env: Env<'a>) -> Ctx<'a> {
        let mask = (env.code.len() / 8) as u32 - 1;
        Ctx {
            r,
            code: env.code,
            mem: Mem { data: env.data },
            top: env.top,
            itop: env.itop,
            kick: env.kick,
            pipe: Pipe::default(),
            cycle: 0,
            ready: [[0; 4]; 32],
            last_vi: None,
            mask,
            id: (0, 0),
        }
    }

    /// Finish a run that ended before instruction `pc`.
    pub fn finish(&mut self, pc: u32) {
        self.pipe.flush(self.r);
        self.r.tpc = pc & self.mask;
    }

    fn words(&self, pc: u32) -> (u32, u32) {
        let at = (pc & self.mask) as usize * 8;
        (u32::from_le_bytes(self.code[at..at + 4].try_into().unwrap()), u32::from_le_bytes(self.code[at + 4..at + 8].try_into().unwrap()))
    }
}

/// Data memory and outside state for compiled programs (ee-recomp's VU
/// compiler keeps registers in locals and the pipeline timing static).
impl Ctx<'_> {
    #[inline(always)]
    pub fn lq(&self, q: usize) -> V4 {
        self.mem.vec(q)
    }
    #[inline(always)]
    pub fn sq(&mut self, q: usize, dest: u8, v: V4) {
        self.mem.store(q, dest, v)
    }
    #[inline(always)]
    pub fn lw(&self, q: usize, n: usize) -> u32 {
        self.mem.word(q, n)
    }
    #[inline(always)]
    pub fn sw(&mut self, q: usize, n: usize, v: u32) {
        self.mem.set_word(q, n, v)
    }
    pub fn top(&self) -> u32 {
        self.top
    }
    pub fn itop(&self) -> u32 {
        self.itop
    }
    pub fn xgkick(&mut self, q: usize) {
        if let Some(k) = self.kick.as_deref_mut() {
            gif_packet(&self.mem, q, k);
        }
    }

    /// Continue in the interpreter at `pc`, the target of the JR/JALR at
    /// `site` that compiled code has no version for. The registers must
    /// already be in `r`; the pipeline state is given as cycles from now:
    /// pending FMAC results (reg, field, cycles), Q, MAC and clip writes
    /// (oldest first), and the VI register the previous pair wrote with its
    /// old value. FREEK_VU_DUMP records the target for the next build.
    #[cold]
    pub fn resume(&mut self, site: u32, pc: u32, ready: &[(usize, usize, u64)], q: Option<(u64, f32)>, mac: &[(u64, u32)], clip: &[(u64, u32)], last_vi: Option<(usize, u16)>) {
        let now = self.cycle;
        for &(reg, f, d) in ready {
            self.ready[reg][f] = now + d;
        }
        self.pipe.q = q.map(|(d, v)| (now + d, v));
        for &(d, v) in mac {
            self.pipe.mac.push(now + d, v, &mut self.r.mac);
        }
        for &(d, v) in clip {
            self.pipe.clip.push(now + d, v, &mut self.r.clip);
        }
        self.last_vi = last_vi;
        record_jump(self.id, site, pc);
        interpret_from(self, pc);
    }
}

/// MAC flags of an FMAC result in the fields of `dest`.
#[inline(always)]
pub fn mac_flags(v: V4, dest: u8) -> u32 {
    fmac_flags(v, dest, 0).0
}

/// CLIP judgement bits of fs.xyz against +/-|ft.w|.
#[inline(always)]
pub fn clip_judge(s: V4, t: V4) -> u32 {
    let w = t[3].abs();
    let mut f = 0;
    for n in 0..3 {
        if s[n] > w {
            f |= 1 << (n * 2);
        }
        if s[n] < -w {
            f |= 2 << (n * 2);
        }
    }
    f
}

/// Execute one instruction pair at `pc` (upper decoded as `m`). Returns
/// the branch target to take after the delay slot, if any. Recompiled
/// programs call this with constant arguments so that decoding folds away.
///
/// `FLAGS` is false for programs that never read the MAC or status flags
/// (see `reads_flags`); their FMAC instructions then skip computing them.
#[inline(always)]
pub fn pair<const FLAGS: bool>(c: &mut Ctx, pc: u32, lo: u32, hi: u32, m: M) -> Option<u32> {
    // Hazard stalls: wait for FMAC results this pair reads, and for the
    // divider before another DIV/SQRT/RSQRT or WAITQ.
    let mut issue = c.cycle;
    for (reg, mask) in reads(lo, hi, m) {
        for f in 0..4 {
            if mask & (8 >> f) != 0 {
                issue = issue.max(c.ready[reg][f]);
            }
        }
    }
    if hi >> 31 == 0 && lo >> 25 == 0x40 && lo & 63 >= 0x3c && matches!(((lo >> 6) & 31) << 2 | (lo & 3), 0x38..=0x3b) {
        if let Some((ready, _)) = c.pipe.q {
            issue = issue.max(ready);
        }
    }
    c.cycle = issue;
    c.pipe.advance_q(c.r, c.cycle);
    // Flag-reading lower instructions see flags as of this cycle.
    let op = lo >> 25;
    if hi >> 31 == 0 && (matches!(op, 0x10 | 0x12..=0x14 | 0x16..=0x1c)) {
        c.pipe.advance(c.r, c.cycle);
    }
    let Ctx { r, mem, pipe, cycle, last_vi, top, itop, kick, mask, ready, .. } = c;
    let r: &mut VuRegs = r;
    let mut delayed = None;
    // ---- lower ----
    let mut lower_vf: Option<(usize, u8, V4)> = None;
    let mut vi_write: Option<(usize, u16)> = None;
    let mut new_i = None;
    if hi >> 31 != 0 {
        new_i = Some(f32::from_bits(lo));
    } else {
        let mut write_vi = |r: &mut VuRegs, n: usize, v: u16| {
            if n != 0 {
                vi_write = Some((n, r.vi[n]));
                r.vi[n] = v;
            }
        };
        let (it, is, id) = (((lo >> 16) & 31) as usize, ((lo >> 11) & 31) as usize, ((lo >> 6) & 31) as usize);
        let (it16, is16) = (it & 15, is & 15);
        let dest = ((lo >> 21) & 15) as u8;
        let imm11 = sext11(lo);
        let vi = |r: &VuRegs, n: usize| r.vi[n & 15];
        // Branch operands see the pre-write value of the last pair's VI.
        let vib = |r: &VuRegs, n: usize| match *last_vi {
            Some((w, old)) if w == n & 15 => old,
            _ => r.vi[n & 15],
        };
        let target = (pc as i32 + 1 + imm11) as u32 & *mask;
        let op = lo >> 25;
        match op {
            0x00 => {
                let a = vi(r, is) as i32 + imm11;
                lower_vf = Some((it, dest, mem.vec(a as usize)));
            }
            0x01 => {
                let a = vi(r, it) as i32 + imm11;
                mem.store(a as usize, dest, r.vf[is]);
            }
            0x04 => {
                let a = (vi(r, is) as i32 + imm11) as usize;
                let v = mem.word(a, dest_field(dest)) as u16;
                write_vi(r, it16, v);
            }
            0x05 => {
                let a = (vi(r, is) as i32 + imm11) as usize;
                let v = vi(r, it) as u32;
                for n in 0..4 {
                    if dest & (8 >> n) != 0 {
                        mem.set_word(a, n, v);
                    }
                }
            }
            0x08 | 0x09 => {
                let imm15 = (lo & 0x7ff) | ((lo >> 21) & 15) << 11;
                let v = if op == 0x08 { vi(r, is).wrapping_add(imm15 as u16) } else { vi(r, is).wrapping_sub(imm15 as u16) };
                write_vi(r, it16, v);
            }
            0x10..=0x13 => {
                let imm24 = lo & 0xff_ffff;
                let clip = r.clip & 0xff_ffff;
                match op {
                    0x10 => write_vi(r, 1, u16::from(clip == imm24)),
                    0x11 => {
                        pipe.clip.clear();
                        r.clip = imm24;
                    }
                    0x12 => write_vi(r, 1, u16::from(clip & imm24 != 0)),
                    _ => write_vi(r, 1, u16::from((clip | imm24) & 0xff_ffff == 0xff_ffff)),
                }
            }
            0x14..=0x17 => {
                let imm12 = (lo & 0x7ff) | ((lo >> 10) & 0x800);
                let st = r.status & 0xfff;
                match op {
                    0x14 => write_vi(r, it16, u16::from(st == imm12)),
                    0x15 => {
                        let s = pipe.status.latest(r.status);
                        r.status = (s & 0x3f) | (imm12 & 0xfc0);
                    }
                    0x16 => write_vi(r, it16, (st & imm12) as u16),
                    _ => write_vi(r, it16, u16::from((st | imm12) & 0xfff == 0xfff)),
                }
            }
            0x18 => write_vi(r, it16, u16::from(r.mac as u16 == vi(r, is))),
            0x1a => write_vi(r, it16, r.mac as u16 & vi(r, is)),
            0x1b => write_vi(r, it16, u16::from((r.mac as u16 | vi(r, is)) == 0xffff)),
            0x1c => write_vi(r, it16, (r.clip & 0xfff) as u16),
            0x20 => delayed = Some(target),
            0x21 => {
                write_vi(r, it16, ((pc + 2) & *mask) as u16);
                delayed = Some(target);
            }
            0x24 => delayed = Some(vib(r, is) as u32 & *mask),
            0x25 => {
                let t = vib(r, is) as u32 & *mask;
                write_vi(r, it16, ((pc + 2) & *mask) as u16);
                delayed = Some(t);
            }
            0x28..=0x2f => {
                let (a, b) = (vib(r, it) as i16, vib(r, is) as i16);
                let cond = match op {
                    0x28 => a == b,
                    0x29 => a != b,
                    0x2c => b < 0,
                    0x2d => b > 0,
                    0x2e => b <= 0,
                    0x2f => b >= 0,
                    _ => false,
                };
                if cond {
                    delayed = Some(target);
                }
            }
            0x40 => {
                let funct = lo & 63;
                if funct < 0x3c {
                    match funct {
                        0x30 => write_vi(r, id & 15, vi(r, is).wrapping_add(vi(r, it))),
                        0x31 => write_vi(r, id & 15, vi(r, is).wrapping_sub(vi(r, it))),
                        0x32 => {
                            let imm5 = (((lo >> 6) & 31) as i8) << 3 >> 3;
                            write_vi(r, it16, vi(r, is).wrapping_add(imm5 as i16 as u16));
                        }
                        0x34 => write_vi(r, id & 15, vi(r, is) & vi(r, it)),
                        0x35 => write_vi(r, id & 15, vi(r, is) | vi(r, it)),
                        _ => {}
                    }
                } else {
                    let fsf = field((lo >> 21) & 3);
                    let ftf = field((lo >> 23) & 3);
                    let (fs, ft) = (r.vf[is], r.vf[it]);
                    match ((lo >> 6) & 31) << 2 | (funct & 3) {
                        0x30 => lower_vf = Some((it, dest, fs)),
                        0x31 => lower_vf = Some((it, dest, vu::mr32(fs))),
                        0x34 => {
                            lower_vf = Some((it, dest, mem.vec(vi(r, is) as usize)));
                            write_vi(r, is16, vi(r, is).wrapping_add(1));
                        }
                        0x35 => {
                            mem.store(vi(r, it) as usize, dest, fs);
                            write_vi(r, it16, vi(r, it).wrapping_add(1));
                        }
                        0x36 => {
                            let a = vi(r, is).wrapping_sub(1);
                            write_vi(r, is16, a);
                            lower_vf = Some((it, dest, mem.vec(a as usize)));
                        }
                        0x37 => {
                            let a = vi(r, it).wrapping_sub(1);
                            write_vi(r, it16, a);
                            mem.store(a as usize, dest, fs);
                        }
                        0x38 | 0x39 | 0x3a => {
                            let (num, den) = (fs[fsf], ft[ftf]);
                            let mut st = pipe.status.latest(r.status) & !0x30;
                            let q = match funct & 3 | ((lo >> 6) & 31) << 2 {
                                0x39 => fsqrt(den),
                                0x38 => {
                                    if den == 0.0 {
                                        st |= if num == 0.0 { 0x10 | 0x400 } else { 0x20 | 0x800 };
                                    }
                                    fdiv(num, den)
                                }
                                _ => {
                                    if den == 0.0 {
                                        st |= 0x20 | 0x800;
                                    }
                                    fdiv(num, fsqrt(den))
                                }
                            };
                            r.status = st;
                            let latency = if (((lo >> 6) & 31) << 2 | (funct & 3)) == 0x3a { 13 } else { 7 };
                            pipe.q = Some((*cycle + latency, q));
                        }
                        0x3b => {
                            if let Some((_, q)) = pipe.q.take() {
                                r.q = q;
                            }
                        }
                        0x3c => write_vi(r, it16, fs[fsf].to_bits() as u16),
                        0x3d => lower_vf = Some((it, dest, [f32::from_bits(vi(r, is) as i16 as i32 as u32); 4])),
                        0x3e => {
                            let v = mem.word(vi(r, is) as usize, dest_field(dest)) as u16;
                            write_vi(r, it16, v);
                        }
                        0x3f => {
                            let (a, v) = (vi(r, is) as usize, vi(r, it) as u32);
                            for n in 0..4 {
                                if dest & (8 >> n) != 0 {
                                    mem.set_word(a, n, v);
                                }
                            }
                        }
                        0x40 => {
                            rnext(r);
                            lower_vf = Some((it, dest, [f32::from_bits(r.r); 4]));
                        }
                        0x41 => lower_vf = Some((it, dest, [f32::from_bits(r.r); 4])),
                        0x42 => r.r = 0x3f80_0000 | (fs[fsf].to_bits() & 0x7f_ffff),
                        0x43 => r.r = 0x3f80_0000 | ((r.r ^ fs[fsf].to_bits()) & 0x7f_ffff),
                        0x64 => lower_vf = Some((it, dest, [r.p; 4])),
                        0x68 => write_vi(r, it16, *top as u16),
                        0x69 => write_vi(r, it16, *itop as u16),
                        0x6c => {
                            if let Some(k) = kick.as_deref_mut() {
                                gif_packet(mem, vi(r, is) as usize, k);
                            }
                        }
                        0x70..=0x7e => r.p = efu(((lo >> 6) & 31) << 2 | (funct & 3), fs, fsf),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // ---- upper ----
    let upper_fd = upper::<FLAGS>(r, pipe, hi, *cycle, m);
    if let Some(reg) = upper_fd {
        let dest = ((hi >> 21) & 15) as u8;
        for f in 0..4 {
            if dest & (8 >> f) != 0 {
                ready[reg][f] = *cycle + FMAC_LATENCY;
            }
        }
    }

    // Lower VF results land after the upper's, unless both target the
    // same register. They go through the same four-cycle pipeline, so a
    // reader stalls for them too; Freekstyle's streak program relies on
    // that stall to let a DIV finish before it reads Q.
    if let Some((reg, dest, v)) = lower_vf {
        if upper_fd != Some(reg) {
            r.vw(reg, dest, v);
            if reg != 0 {
                for f in 0..4 {
                    if dest & (8 >> f) != 0 {
                        ready[reg][f] = *cycle + FMAC_LATENCY;
                    }
                }
            }
        }
    }
    if let Some(i) = new_i {
        r.i = i;
    }
    // Only integer ALU results and loads reach the branch unit late; flag
    // checks (FCAND, FSAND, FMAND, ...) are seen at once. Freekstyle's
    // clipper tests FCOR results with the very next IBNE.
    let flag_op = hi >> 31 == 0 && (0x10..=0x1c).contains(&(lo >> 25));
    *last_vi = if flag_op { None } else { vi_write };
    *cycle += 1;
    delayed
}

/// Continue interpreting from `pc` (used by recompiled code for jumps it
/// does not cover).
pub fn interpret_from(c: &mut Ctx, pc: u32) {
    let mut cache = DecodeCache::default();
    cache.sync(1, c.code.len() / 8);
    interpret(c, &mut cache, pc);
}

/// Interpret from `pc` until the program ends (E bit plus one pair).
pub fn interpret(c: &mut Ctx, cache: &mut DecodeCache, mut pc: u32) {
    let code = c.code;
    let flags = *cache.flags.get_or_insert_with(|| reads_flags(code));
    let mut delayed: Option<u32> = None;
    let mut ending = false;
    let start = pc;
    loop {
        pc &= c.mask;
        let (lo, hi) = c.words(pc);
        let m = *cache.upper[pc as usize].get_or_insert_with(|| decode(0x4a00_0000 | (hi & 0x01ff_ffff)).m);
        let take = delayed.take();
        let branch = if flags { pair::<true>(c, pc, lo, hi, m) } else { pair::<false>(c, pc, lo, hi, m) };
        if ending || c.cycle > MAX_CYCLES {
            if c.cycle > MAX_CYCLES {
                eprintln!("[vu] microprogram at {start:#x} did not end; stopped");
            }
            // A branch taken in the E-bit pair decides where MSCNT resumes.
            c.finish(take.unwrap_or(pc + 1));
            return;
        }
        if hi & (1 << 30) != 0 {
            ending = true;
        }
        pc = take.unwrap_or(pc + 1);
        delayed = branch;
    }
}

/// Execute an upper instruction. Returns the VF register it wrote, if any.
#[inline(always)]
fn upper<const FLAGS: bool>(r: &mut VuRegs, pipe: &mut Pipe, hi: u32, cycle: u64, m: M) -> Option<usize> {
    // Upper words use the COP2 macro encoding.
    let i = r5900::Insn { raw: 0x4a00_0000 | (hi & 0x01ff_ffff), m };
    let (fs, ft, fd) = (i.fs() as usize, i.ft() as usize, i.fd() as usize);
    let dest = i.dest();
    let (s, t) = (r.vf[fs], r.vf[ft]);
    let bc = vu::splat(t[i.bc() as usize]);
    let (q, ii, acc) = (vu::splat(r.q), vu::splat(r.i), r.acc);
    enum Out {
        Fd(V4),
        Acc(V4),
        Ft(V4),
        None,
    }
    let (out, flags) = match i.m {
        M::VaddBc => (Out::Fd(vu::add(s, bc)), true),
        M::VsubBc => (Out::Fd(vu::sub(s, bc)), true),
        M::VmulBc => (Out::Fd(vu::mul(s, bc)), true),
        M::VmaddBc => (Out::Fd(vu::madd(acc, s, bc)), true),
        M::VmsubBc => (Out::Fd(vu::msub(acc, s, bc)), true),
        M::VmaxBc => (Out::Fd(vu::max(s, bc)), false),
        M::VminiBc => (Out::Fd(vu::mini(s, bc)), false),
        M::Vadd => (Out::Fd(vu::add(s, t)), true),
        M::Vsub => (Out::Fd(vu::sub(s, t)), true),
        M::Vmul => (Out::Fd(vu::mul(s, t)), true),
        M::Vmadd => (Out::Fd(vu::madd(acc, s, t)), true),
        M::Vmsub => (Out::Fd(vu::msub(acc, s, t)), true),
        M::Vmax => (Out::Fd(vu::max(s, t)), false),
        M::Vmini => (Out::Fd(vu::mini(s, t)), false),
        M::Vaddq => (Out::Fd(vu::add(s, q)), true),
        M::Vsubq => (Out::Fd(vu::sub(s, q)), true),
        M::Vmulq => (Out::Fd(vu::mul(s, q)), true),
        M::Vmaddq => (Out::Fd(vu::madd(acc, s, q)), true),
        M::Vmsubq => (Out::Fd(vu::msub(acc, s, q)), true),
        M::Vaddi => (Out::Fd(vu::add(s, ii)), true),
        M::Vsubi => (Out::Fd(vu::sub(s, ii)), true),
        M::Vmuli => (Out::Fd(vu::mul(s, ii)), true),
        M::Vmaddi => (Out::Fd(vu::madd(acc, s, ii)), true),
        M::Vmsubi => (Out::Fd(vu::msub(acc, s, ii)), true),
        M::Vmaxi => (Out::Fd(vu::max(s, ii)), false),
        M::Vminii => (Out::Fd(vu::mini(s, ii)), false),
        M::Vopmsub => (Out::Fd(vu::sub(acc, vu::cross(s, t))), true),
        M::VaddaBc => (Out::Acc(vu::add(s, bc)), true),
        M::VsubaBc => (Out::Acc(vu::sub(s, bc)), true),
        M::VmulaBc => (Out::Acc(vu::mul(s, bc)), true),
        M::VmaddaBc => (Out::Acc(vu::madd(acc, s, bc)), true),
        M::VmsubaBc => (Out::Acc(vu::msub(acc, s, bc)), true),
        M::Vadda => (Out::Acc(vu::add(s, t)), true),
        M::Vsuba => (Out::Acc(vu::sub(s, t)), true),
        M::Vmula => (Out::Acc(vu::mul(s, t)), true),
        M::Vmadda => (Out::Acc(vu::madd(acc, s, t)), true),
        M::Vmsuba => (Out::Acc(vu::msub(acc, s, t)), true),
        M::Vaddaq => (Out::Acc(vu::add(s, q)), true),
        M::Vsubaq => (Out::Acc(vu::sub(s, q)), true),
        M::Vmulaq => (Out::Acc(vu::mul(s, q)), true),
        M::Vmaddaq => (Out::Acc(vu::madd(acc, s, q)), true),
        M::Vmsubaq => (Out::Acc(vu::msub(acc, s, q)), true),
        M::Vaddai => (Out::Acc(vu::add(s, ii)), true),
        M::Vsubai => (Out::Acc(vu::sub(s, ii)), true),
        M::Vmulai => (Out::Acc(vu::mul(s, ii)), true),
        M::Vmaddai => (Out::Acc(vu::madd(acc, s, ii)), true),
        M::Vmsubai => (Out::Acc(vu::msub(acc, s, ii)), true),
        M::Vopmula => (Out::Acc(vu::cross(s, t)), true),
        M::Vabs => (Out::Ft(vu::abs(s)), false),
        M::Vitof0 => (Out::Ft(vu::itof(s, 0)), false),
        M::Vitof4 => (Out::Ft(vu::itof(s, 4)), false),
        M::Vitof12 => (Out::Ft(vu::itof(s, 12)), false),
        M::Vitof15 => (Out::Ft(vu::itof(s, 15)), false),
        M::Vftoi0 => (Out::Ft(vu::ftoi(s, 0)), false),
        M::Vftoi4 => (Out::Ft(vu::ftoi(s, 4)), false),
        M::Vftoi12 => (Out::Ft(vu::ftoi(s, 12)), false),
        M::Vftoi15 => (Out::Ft(vu::ftoi(s, 15)), false),
        M::Vclipw => {
            let clip = ((pipe.clip.latest(r.clip) << 6) | clip_judge(s, t)) & 0xff_ffff;
            pipe.clip.push(cycle + FLAG_LATENCY, clip, &mut r.clip);
            (Out::None, false)
        }
        _ => (Out::None, false),
    };
    // (VF register written, result for the flags)
    let (reg, result) = match out {
        Out::Fd(v) => {
            r.vw(fd, dest, v);
            (Some(fd), Some(v))
        }
        Out::Acc(v) => {
            r.accw(dest, v);
            (None, Some(v))
        }
        Out::Ft(v) => {
            r.vw(ft, dest, v);
            (Some(ft), Some(v))
        }
        Out::None => (None, None),
    };
    if let (true, true, Some(v)) = (FLAGS, flags, result) {
        let (mac, status) = fmac_flags(v, dest, pipe.status.latest(r.status));
        pipe.mac.push(cycle + FLAG_LATENCY, mac, &mut r.mac);
        pipe.status.push(cycle + FLAG_LATENCY, status, &mut r.status);
    }
    reg
}

/// The VU's 23-bit random number generator.
fn rnext(r: &mut VuRegs) {
    r.r = rnext_value(r.r);
}

pub fn rnext_value(r: u32) -> u32 {
    let x = r & 0x7f_ffff;
    let bit = ((x >> 4) ^ (x >> 22)) & 1;
    0x3f80_0000 | (((x << 1) | bit) & 0x7f_ffff)
}

/// Elementary function unit; results go to P.
pub fn efu(op: u32, s: V4, fsf: usize) -> f32 {
    let len2 = s[0] * s[0] + s[1] * s[1] + s[2] * s[2];
    ps2f(match op {
        0x70 => len2,
        0x71 => 1.0 / len2,
        0x72 => len2.sqrt(),
        0x73 => 1.0 / len2.sqrt(),
        0x74 => (s[1] / s[0]).atan(),
        0x75 => (s[2] / s[0]).atan(),
        0x76 => s[0] + s[1] + s[2] + s[3],
        0x78 => fsqrt(s[fsf]),
        0x79 => 1.0 / fsqrt(s[fsf]),
        0x7a => 1.0 / s[fsf],
        0x7c => s[fsf].sin(),
        0x7d => s[fsf].atan(),
        0x7e => (-s[fsf]).exp(),
        _ => 0.0,
    })
}

/// Append a GIF packet (up to EOP) from VU memory for XGKICK.
fn gif_packet(mem: &Mem, start: usize, out: &mut Vec<u128>) {
    crate::dma::PATH1_PACKETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let len0 = out.len();
    let mut q = start;
    loop {
        let tag = mem.qword(q);
        out.push(tag);
        q += 1;
        let nloop = (tag & 0x7fff) as usize;
        let eop = tag >> 15 & 1 != 0;
        let flg = (tag >> 58) & 3;
        let nreg = match (tag >> 60) & 15 {
            0 => 16,
            n => n as usize,
        };
        let n = match flg {
            0 => nloop * nreg,
            1 => (nloop * nreg).div_ceil(2),
            _ => nloop,
        };
        // A runaway packet stops once it has covered all of memory.
        let n = n.min(mem.qwords() + 1);
        mem.extend_qwords(q, n, out);
        q += n;
        if eop || out.len() - len0 > mem.qwords() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: u32 = 1 << 30;
    const NOP: u32 = 0x2ff; // upper VNOP
    const NOPL: u32 = 0x8000_0330; // lower NOP (IOR vi0, vi0, vi0)

    fn lq(ft: u32, is: u32, imm: i32, dest: u32) -> u32 {
        dest << 21 | ft << 16 | is << 11 | (imm as u32 & 0x7ff)
    }
    fn sq(fs: u32, it: u32, imm: i32, dest: u32) -> u32 {
        1 << 25 | dest << 21 | it << 16 | fs << 11 | (imm as u32 & 0x7ff)
    }
    fn iaddiu(it: u32, is: u32, imm: u32) -> u32 {
        0x08 << 25 | (imm >> 11) << 21 | it << 16 | is << 11 | (imm & 0x7ff)
    }
    fn ibne(it: u32, is: u32, off: i32) -> u32 {
        0x29 << 25 | it << 16 | is << 11 | (off as u32 & 0x7ff)
    }
    fn vadd(fd: u32, fs: u32, ft: u32) -> u32 {
        0xf << 21 | ft << 16 | fs << 11 | fd << 6 | 40
    }
    fn vmulq(fd: u32, fs: u32) -> u32 {
        0xf << 21 | fs << 11 | fd << 6 | 28
    }
    fn div(fs: u32, fsf: u32, ft: u32, ftf: u32) -> u32 {
        0x40 << 25 | ftf << 23 | fsf << 21 | ft << 16 | fs << 11 | (0x38 >> 2) << 6 | (0x38 & 3) | 0x3c
    }

    fn program(pairs: &[(u32, u32)]) -> Vec<u8> {
        let mut code = vec![0u8; 16 << 10];
        for (n, &(upper, lower)) in pairs.iter().enumerate() {
            code[n * 8..n * 8 + 4].copy_from_slice(&lower.to_le_bytes());
            code[n * 8 + 4..n * 8 + 8].copy_from_slice(&upper.to_le_bytes());
        }
        code
    }

    fn exec(pairs: &[(u32, u32)], data: &mut [u8], r: &mut VuRegs) -> Vec<u128> {
        let code = program(pairs);
        let mut kicked = Vec::new();
        let mut cache = DecodeCache::default();
        let env = Env { code: &code, data, top: 0, itop: 0, kick: Some(&mut kicked) };
        run(r, env, &mut cache, 1, &Programs(&[]), 0);
        kicked
    }

    fn put(data: &mut [u8], q: usize, v: V4) {
        for (n, x) in v.iter().enumerate() {
            data[q * 16 + n * 4..q * 16 + n * 4 + 4].copy_from_slice(&x.to_bits().to_le_bytes());
        }
    }

    fn get(data: &[u8], q: usize) -> V4 {
        std::array::from_fn(|n| f32::from_bits(u32::from_le_bytes(data[q * 16 + n * 4..q * 16 + n * 4 + 4].try_into().unwrap())))
    }

    #[test]
    fn load_add_store_and_end() {
        let mut data = vec![0u8; 16 << 10];
        put(&mut data, 0, [1.0, 2.0, 3.0, 4.0]);
        put(&mut data, 1, [10.0, 20.0, 30.0, 40.0]);
        let mut r = VuRegs::new();
        exec(
            &[
                (NOP, lq(1, 0, 0, 0xf)),
                (NOP, lq(2, 0, 1, 0xf)),
                (vadd(3, 1, 2), NOPL),
                (NOP | E, sq(3, 0, 2, 0xf)),
                (NOP, NOPL),
                // Never reached: after E only the next pair runs.
                (NOP, sq(1, 0, 3, 0xf)),
            ],
            &mut data,
            &mut r,
        );
        assert_eq!(get(&data, 2), [11.0, 22.0, 33.0, 44.0]);
        assert_eq!(get(&data, 3), [0.0; 4]);
        assert_eq!(r.tpc, 5);
    }

    #[test]
    fn loop_with_delay_slot() {
        // vi1 counts to 3; the delay slot increments vi2 every iteration.
        let mut data = vec![0u8; 16 << 10];
        let mut r = VuRegs::new();
        exec(
            &[
                (NOP, iaddiu(3, 0, 3)),
                (NOP, iaddiu(1, 1, 1)), // loop:
                (NOP, NOPL),
                (NOP, ibne(1, 3, -3)),
                (NOP, iaddiu(2, 2, 1)), // delay slot
                (NOP | E, NOPL),
                (NOP, NOPL),
            ],
            &mut data,
            &mut r,
        );
        assert_eq!((r.vi[1], r.vi[2]), (3, 3));
    }

    #[test]
    fn branch_sees_old_vi_after_write() {
        // IBNE right after IADDIU on the same register uses the old value:
        // vi1 was 0 == vi0, so the branch is not taken.
        let mut data = vec![0u8; 16 << 10];
        let mut r = VuRegs::new();
        exec(
            &[(NOP, iaddiu(1, 0, 5)), (NOP, ibne(1, 0, 3)), (NOP, NOPL), (NOP | E, iaddiu(4, 0, 1)), (NOP, NOPL), (NOP, NOPL), (NOP | E, iaddiu(4, 0, 2)), (NOP, NOPL)],
            &mut data,
            &mut r,
        );
        assert_eq!(r.vi[4], 1);
    }

    #[test]
    fn branch_sees_flag_check_result_at_once() {
        // FCSET 1; FCOR vi1 = (clip | 0xfffffe) == 0xffffff = 1; IBNE vi1
        // right after it must see the new value and branch.
        let fcset = |v: u32| 0x11 << 25 | v;
        let fcor = |v: u32| 0x13 << 25 | v;
        let mut data = vec![0u8; 16 << 10];
        let mut r = VuRegs::new();
        exec(
            &[(NOP, fcset(1)), (NOP, fcor(0xff_fffe)), (NOP, ibne(1, 0, 3)), (NOP, NOPL), (NOP | E, iaddiu(4, 0, 1)), (NOP, NOPL), (NOP, NOPL), (NOP | E, iaddiu(4, 0, 2)), (NOP, NOPL)],
            &mut data,
            &mut r,
        );
        assert_eq!(r.vi[4], 2);
    }

    #[test]
    fn q_has_divider_latency() {
        let mut data = vec![0u8; 16 << 10];
        put(&mut data, 0, [8.0, 2.0, 0.0, 0.0]);
        let mut r = VuRegs::new();
        r.q = 100.0;
        exec(
            &[
                (NOP, lq(1, 0, 0, 0xf)),
                (NOP, div(1, 0, 1, 1)),        // Q = 8 / 2, ready 7 cycles later
                (vmulq(2, 1), NOPL),           // still the old Q (100)
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (vmulq(3, 1), NOPL),           // new Q (4)
                (NOP | E, NOPL),
                (NOP, NOPL),
            ],
            &mut data,
            &mut r,
        );
        assert_eq!(r.vf[2][0], 800.0);
        assert_eq!(r.vf[3][0], 32.0);
    }

    #[test]
    fn hazard_stall_delivers_q_in_time() {
        // The pattern of the game's transform loop: DIV, then an FMAC read
        // of a register written two pairs earlier (stalls 2 cycles), so a
        // MULQ six pairs after the DIV already sees the new Q.
        let mut data = vec![0u8; 16 << 10];
        put(&mut data, 0, [8.0, 2.0, 0.0, 0.0]);
        let mut r = VuRegs::new();
        r.q = 100.0;
        let mini_w = 0x1 << 21 | 1 << 16 | 1 << 11 | 5 << 6 | 47; // mini.w vf5, vf1, vf1
        let mul_w = 0x1 << 21 | 5 << 16 | 1 << 11 | 6 << 6 | 42; // mul.w vf6, vf1, vf5 (needs vf5)
        exec(
            &[
                (NOP, lq(1, 0, 0, 0xf)),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (mini_w, div(1, 0, 1, 1)), // Q = 4 after 7 cycles
                (NOP, NOPL),
                (mul_w, NOPL), // stalls 2 cycles on vf5
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (vmulq(3, 1), NOPL), // 6 pairs after DIV, 8 cycles: new Q
                (NOP | E, NOPL),
                (NOP, NOPL),
            ],
            &mut data,
            &mut r,
        );
        assert_eq!(r.vf[3][0], 32.0);
    }

    #[test]
    fn lower_vf_write_stalls_reader() {
        // The game's streak program: DIV, then MR32 (a lower op) writes a
        // register the MULQ two pairs later reads. The read stalls until
        // the MR32 result lands, by which time the new Q is there; without
        // the stall a trail point is projected with a stale Q.
        let mut data = vec![0u8; 16 << 10];
        put(&mut data, 0, [8.0, 2.0, 0.0, 0.0]);
        let mut r = VuRegs::new();
        r.q = 100.0;
        let mr32 = |ft: u32, fs: u32| 0x40 << 25 | 0xf << 21 | ft << 16 | fs << 11 | 0x33d;
        exec(
            &[
                (NOP, lq(1, 0, 0, 0xf)),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, div(1, 0, 1, 1)), // Q = 4 after 7 cycles
                (NOP, NOPL),
                (NOP, NOPL),
                (NOP, mr32(5, 1)), // vf5 = [2, 0, 0, 8]
                (NOP, NOPL),
                (vmulq(3, 5), NOPL), // stalls 2 cycles on vf5: new Q
                (NOP | E, NOPL),
                (NOP, NOPL),
            ],
            &mut data,
            &mut r,
        );
        assert_eq!(r.vf[3][0], 8.0);
    }

    #[test]
    fn end_with_branch_resumes_at_target() {
        // E bit on a pair that branches: the program stops after the delay
        // slot, and TPC (where MSCNT continues) is the branch target.
        let mut data = vec![0u8; 16 << 10];
        let mut r = VuRegs::new();
        let b = |off: i32| 0x20 << 25 | (off as u32 & 0x7ff);
        exec(&[(NOP, NOPL), (NOP | E, b(-2)), (NOP, NOPL), (NOP, NOPL)], &mut data, &mut r);
        assert_eq!(r.tpc, 0);
    }

    #[test]
    fn xgkick_collects_packet() {
        let mut data = vec![0u8; 16 << 10];
        // PACKED tag: NLOOP 2, NREG 1, EOP, then two data qwords.
        let tag: u128 = 2 | 1 << 15 | 1u128 << 60;
        data[0..16].copy_from_slice(&tag.to_le_bytes());
        data[16] = 0xaa;
        data[32] = 0xbb;
        let xgkick = 0x40 << 25 | (0x6c >> 2) << 6 | 0x3c | (0x6c & 3);
        let mut r = VuRegs::new();
        let kicked = exec(&[(NOP | E, xgkick), (NOP, NOPL)], &mut data, &mut r);
        assert_eq!(kicked.len(), 3);
        assert_eq!(kicked[2] & 0xff, 0xbb);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// cargo test --release -p ee-runtime vumicro::bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pair_throughput() {
        // A loop of VADD pairs: vi1 counts down from 2^15.
        let vadd = 0xf << 21 | 2 << 16 | 1 << 11 | 3 << 6 | 40u32;
        let nop_lower = 0x8000_0330u32;
        let mut code = vec![0u8; 16 << 10];
        let mut put = |n: usize, upper: u32, lower: u32| {
            code[n * 8..n * 8 + 4].copy_from_slice(&lower.to_le_bytes());
            code[n * 8 + 4..n * 8 + 8].copy_from_slice(&upper.to_le_bytes());
        };
        put(0, 0x2ff, 0x08 << 25 | 1 << 16 | 0x7fff); // iaddiu vi1, vi0, 0x7fff
        for n in 1..30 {
            put(n, vadd, nop_lower);
        }
        // isubiu vi1, vi1, 1 ; ibne vi1, vi0, -30 ; nop ; E
        put(30, 0x2ff, 0x09 << 25 | 1 << 16 | 1 << 11 | 1);
        put(31, 0x2ff, 0x29 << 25 | 1 << 16 | (-31i32 as u32 & 0x7ff));
        put(32, vadd, nop_lower);
        put(33, 0x2ff | 1 << 30, nop_lower);
        put(34, 0x2ff, nop_lower);
        let mut data = vec![0u8; 16 << 10];
        let mut r = VuRegs::new();
        let mut cache = DecodeCache::default();
        let t = std::time::Instant::now();
        let env = Env { code: &code, data: &mut data, top: 0, itop: 0, kick: None };
        let cycles = run(&mut r, env, &mut cache, 1, &Programs(&[]), 0);
        let ns = t.elapsed().as_nanos() as f64 / cycles as f64;
        eprintln!("{cycles} pairs, {ns:.1} ns/pair");
    }
}

#[cfg(test)]
mod bench_parts {
    use super::*;
    use std::hint::black_box;

    #[test]
    #[ignore]
    fn parts() {
        let n = 10_000_000u64;
        let time = |name: &str, f: &mut dyn FnMut()| {
            let t = std::time::Instant::now();
            for _ in 0..n {
                f();
            }
            eprintln!("{name:<14} {:.1} ns", t.elapsed().as_nanos() as f64 / n as f64);
        };
        let mut r = VuRegs::new();
        r.vf[1] = [1.0, 2.0, 3.0, 4.0];
        time("vu::add", &mut || {
            black_box(vu::add(black_box(r.vf[1]), black_box(r.vf[2])));
        });
        time("fmac_flags", &mut || {
            black_box(fmac_flags(black_box([1.0, 0.0, -2.0, 3.0]), 15, black_box(0)));
        });
        let mut pipe = Pipe::default();
        let mut cyc = 0u64;
        time("pipe push+adv", &mut || {
            cyc += 1;
            pipe.mac.push(cyc + 4, 1, &mut r.mac);
            pipe.status.push(cyc + 4, 2, &mut r.status);
        });
        let hi = 0xf << 21 | 2 << 16 | 1 << 11 | 3 << 6 | 40u32;
        let m = decode(0x4a00_0000 | (hi & 0x01ff_ffff)).m;
        let mut pipe = Pipe::default();
        let mut cyc = 0u64;
        time("upper()", &mut || {
            cyc += 1;
            black_box(upper::<true>(&mut r, &mut pipe, black_box(hi), cyc, m));
        });
        time("decode()", &mut || {
            black_box(decode(black_box(0x4a00_0000 | (hi & 0x01ff_ffff))));
        });
    }
}

fn check_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FREEK_VU_CHECK").is_some())
}

/// FREEK_VU_CHECK: run each compiled program, then the interpreter from the
/// same registers and memory, and report any difference in registers, data
/// memory or XGKICKed packets. The status flag is not compared (compiled
/// code skips it; this game never reads it), nor MAC for programs that never
/// read MAC. The compiled results are kept.
#[cold]
fn check(f: NativeFn, r: &mut VuRegs, env: Env, cache: &mut DecodeCache, unit: u8, hash: u64, start: u32) -> u64 {
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static BAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    use std::sync::atomic::Ordering::Relaxed;
    let Env { code, data, top, itop, mut kick } = env;
    let (r0, d0) = (r.clone(), data.to_vec());
    let (mut got, mut want) = (Vec::new(), Vec::new());
    let mut c = Ctx::new(&mut *r, Env { code, data: &mut *data, top, itop, kick: Some(&mut got) });
    f(&mut c);
    let cycles = c.cycle;
    let (mut ri, mut di) = (r0, d0);
    let mut ci = Ctx::new(&mut ri, Env { code, data: &mut di, top, itop, kick: Some(&mut want) });
    interpret(&mut ci, cache, start);
    let mut diffs = Vec::new();
    let reads_mac = code.chunks_exact(8).any(|p| p[7] >> 7 == 0 && matches!(p[3] >> 1, 0x18 | 0x1a | 0x1b));
    for n in 1..32 {
        if r.vf[n].map(f32::to_bits) != ri.vf[n].map(f32::to_bits) {
            diffs.push(format!("vf{n} {:?} want {:?}", r.vf[n], ri.vf[n]));
        }
    }
    for n in 1..16 {
        if r.vi[n] != ri.vi[n] {
            diffs.push(format!("vi{n} {:#x} want {:#x}", r.vi[n], ri.vi[n]));
        }
    }
    let scalars = [
        ("acc", r.acc.map(f32::to_bits) != ri.acc.map(f32::to_bits)),
        ("q", r.q.to_bits() != ri.q.to_bits()),
        ("i", r.i.to_bits() != ri.i.to_bits()),
        ("p", r.p.to_bits() != ri.p.to_bits()),
        ("r", r.r != ri.r),
        ("clip", r.clip != ri.clip),
        ("mac", reads_mac && r.mac != ri.mac),
        ("tpc", r.tpc != ri.tpc),
    ];
    diffs.extend(scalars.iter().filter(|s| s.1).map(|s| s.0.to_string()));
    if let Some(q) = (0..data.len() / 16).find(|&q| data[q * 16..q * 16 + 16] != di[q * 16..q * 16 + 16]) {
        diffs.push(format!("data qword {q:#x}"));
    }
    if got != want {
        diffs.push(format!("XGKICK packets ({} vs {} qwords)", got.len(), want.len()));
    }
    let runs = RUNS.fetch_add(1, Relaxed) + 1;
    if !diffs.is_empty() && BAD.fetch_add(1, Relaxed) < 30 {
        eprintln!("[vu-check] vu{unit} {hash:016x}@{start:#x}: {}", diffs.join(", "));
    }
    if runs.is_power_of_two() && runs >= 1024 {
        eprintln!("[vu-check] {runs} runs checked, {} differed", BAD.load(Relaxed));
    }
    if let Some(k) = kick.as_deref_mut() {
        k.extend(got);
    }
    cycles
}

/// FREEK_VU_TOP: count runs per entry point; print the busiest every
/// 600 runs of VU1 (debugging aid).
#[cold]
fn vu_top(unit: u8, hash: u64, start: u32) {
    use std::collections::HashMap;
    static COUNTS: std::sync::Mutex<Option<(u64, HashMap<(u8, u64, u32), u64>)>> = std::sync::Mutex::new(None);
    let mut g = COUNTS.lock().unwrap();
    let (n, m) = g.get_or_insert_with(|| (0, HashMap::new()));
    *m.entry((unit, hash, start)).or_default() += 1;
    *n += 1;
    if *n % 200_000 == 0 {
        let mut v: Vec<_> = m.drain().collect();
        v.sort_by_key(|e| std::cmp::Reverse(e.1));
        let top: Vec<String> = v.iter().take(10).map(|((u, h, s), c)| format!("vu{u} {h:016x}@{s:#x}:{c}")).collect();
        eprintln!("[vu-top] {}", top.join(" "));
    }
}
