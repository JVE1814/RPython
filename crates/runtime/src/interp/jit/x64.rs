//! Minimal x86-64 machine-code assembler (our own; no codegen crates).
//! Only the instruction forms the JIT tiers need. Memory operands are always
//! `[base + disp]` (disp8/disp32 chosen automatically).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum R {
    Rax = 0,
    Rcx,
    Rdx,
    Rbx,
    Rsp,
    Rbp,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
}
pub use R::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct X(pub u8);
pub const XMM0: X = X(0);
pub const XMM1: X = X(1);
pub const XMM2: X = X(2);

/// Memory operand [base + disp].
#[derive(Clone, Copy, Debug)]
pub struct M(pub R, pub i32);

/// Condition codes (low nibble of Jcc/SETcc).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Cc {
    O = 0,
    NO = 1,
    B = 2,
    AE = 3,
    E = 4,
    NE = 5,
    BE = 6,
    A = 7,
    S = 8,
    NS = 9,
    P = 0xa,
    NP = 0xb,
    L = 0xc,
    GE = 0xd,
    LE = 0xe,
    G = 0xf,
}

impl Cc {
    pub fn not(self) -> Cc {
        unsafe { std::mem::transmute(self as u8 ^ 1) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Label(pub u32);

pub struct Asm {
    pub buf: Vec<u8>,
    labels: Vec<Option<usize>>,
    /// (position of rel32 field, label)
    fixups: Vec<(usize, Label)>,
    /// (position of imm64 field, label): absolute address of label, patched at finalize
    abs_fixups: Vec<(usize, Label)>,
}

impl Default for Asm {
    fn default() -> Self {
        Self::new()
    }
}

impl Asm {
    pub fn new() -> Asm {
        Asm { buf: Vec::with_capacity(4096), labels: Vec::new(), fixups: Vec::new(), abs_fixups: Vec::new() }
    }
    #[inline]
    pub fn pos(&self) -> usize {
        self.buf.len()
    }
    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() as u32 - 1)
    }
    pub fn bind(&mut self, l: Label) {
        debug_assert!(self.labels[l.0 as usize].is_none());
        self.labels[l.0 as usize] = Some(self.buf.len());
    }
    pub fn is_bound(&self, l: Label) -> bool {
        self.labels[l.0 as usize].is_some()
    }
    pub fn label_pos(&self, l: Label) -> Option<usize> {
        self.labels[l.0 as usize]
    }

    #[inline]
    fn b(&mut self, x: u8) {
        self.buf.push(x);
    }
    #[inline]
    fn d32(&mut self, x: i32) {
        self.buf.extend_from_slice(&x.to_le_bytes());
    }
    #[inline]
    fn d64(&mut self, x: u64) {
        self.buf.extend_from_slice(&x.to_le_bytes());
    }

    fn rex(&mut self, w: bool, r: u8, x: u8, b: u8, force: bool) {
        let v = 0x40 | (w as u8) << 3 | ((r >> 3) & 1) << 2 | ((x >> 3) & 1) << 1 | ((b >> 3) & 1);
        if v != 0x40 || force {
            self.b(v);
        }
    }

    /// ModRM (+SIB, disp) for reg field `reg` and memory operand m.
    fn modrm_mem(&mut self, reg: u8, m: M) {
        let base = m.0 as u8;
        let (md, disp8) = if m.1 == 0 && (base & 7) != 5 {
            (0u8, None)
        } else if (-128..=127).contains(&m.1) {
            (1u8, Some(m.1 as i8))
        } else {
            (2u8, None)
        };
        self.b(md << 6 | (reg & 7) << 3 | (base & 7));
        if base & 7 == 4 {
            self.b(0x24);
        }
        match md {
            1 => self.b(disp8.unwrap() as u8),
            2 => self.d32(m.1),
            _ => {}
        }
    }
    fn modrm_reg(&mut self, reg: u8, rm: u8) {
        self.b(0xc0 | (reg & 7) << 3 | (rm & 7));
    }

    // ------------------------------------------------------------------ moves
    pub fn mov_rr(&mut self, dst: R, src: R) {
        self.rex(true, src as u8, 0, dst as u8, false);
        self.b(0x89);
        self.modrm_reg(src as u8, dst as u8);
    }
    pub fn mov_ri(&mut self, dst: R, imm: i64) {
        if imm == 0 {
            // xor r32, r32
            self.rex(false, dst as u8, 0, dst as u8, false);
            self.b(0x31);
            self.modrm_reg(dst as u8, dst as u8);
        } else if (0..=u32::MAX as i64).contains(&imm) {
            self.rex(false, 0, 0, dst as u8, false);
            self.b(0xb8 + (dst as u8 & 7));
            self.d32(imm as u32 as i32);
        } else if (i32::MIN as i64..=i32::MAX as i64).contains(&imm) {
            self.rex(true, 0, 0, dst as u8, false);
            self.b(0xc7);
            self.modrm_reg(0, dst as u8);
            self.d32(imm as i32);
        } else {
            self.rex(true, 0, 0, dst as u8, false);
            self.b(0xb8 + (dst as u8 & 7));
            self.d64(imm as u64);
        }
    }
    /// mov r64, imm64 whose value is the absolute address of a label (patched later).
    pub fn mov_r_label_addr(&mut self, dst: R, l: Label) {
        self.rex(true, 0, 0, dst as u8, false);
        self.b(0xb8 + (dst as u8 & 7));
        self.abs_fixups.push((self.pos(), l));
        self.d64(0);
    }
    pub fn load(&mut self, dst: R, m: M) {
        self.rex(true, dst as u8, 0, m.0 as u8, false);
        self.b(0x8b);
        self.modrm_mem(dst as u8, m);
    }
    pub fn store(&mut self, m: M, src: R) {
        self.rex(true, src as u8, 0, m.0 as u8, false);
        self.b(0x89);
        self.modrm_mem(src as u8, m);
    }
    /// 32-bit load (zero-extends)
    pub fn load32(&mut self, dst: R, m: M) {
        self.rex(false, dst as u8, 0, m.0 as u8, false);
        self.b(0x8b);
        self.modrm_mem(dst as u8, m);
    }
    pub fn store32(&mut self, m: M, src: R) {
        self.rex(false, src as u8, 0, m.0 as u8, false);
        self.b(0x89);
        self.modrm_mem(src as u8, m);
    }
    /// movzx r32, byte [m]
    pub fn load8(&mut self, dst: R, m: M) {
        self.rex(false, dst as u8, 0, m.0 as u8, false);
        self.b(0x0f);
        self.b(0xb6);
        self.modrm_mem(dst as u8, m);
    }
    /// mov qword [m], imm32 (sign-extended)
    pub fn store_i(&mut self, m: M, imm: i32) {
        self.rex(true, 0, 0, m.0 as u8, false);
        self.b(0xc7);
        self.modrm_mem(0, m);
        self.d32(imm);
    }
    /// mov dword [m], imm32
    pub fn store32_i(&mut self, m: M, imm: i32) {
        self.rex(false, 0, 0, m.0 as u8, false);
        self.b(0xc7);
        self.modrm_mem(0, m);
        self.d32(imm);
    }
    pub fn lea(&mut self, dst: R, m: M) {
        self.rex(true, dst as u8, 0, m.0 as u8, false);
        self.b(0x8d);
        self.modrm_mem(dst as u8, m);
    }
    /// mov r64, fs:[0]
    pub fn load_fs0(&mut self, dst: R) {
        self.b(0x64);
        self.rex(true, dst as u8, 0, 0, false);
        self.b(0x8b);
        // mod=00 rm=100 (SIB) base=101 (disp32, no base) index=100 (none)
        self.b((dst as u8 & 7) << 3 | 0x04);
        self.b(0x25);
        self.d32(0);
    }

    // ------------------------------------------------------------------ ALU
    fn alu_rr(&mut self, op: u8, dst: R, src: R) {
        self.rex(true, src as u8, 0, dst as u8, false);
        self.b(op);
        self.modrm_reg(src as u8, dst as u8);
    }
    fn alu_ri(&mut self, ext: u8, dst: R, imm: i32) {
        self.rex(true, 0, 0, dst as u8, false);
        if (-128..=127).contains(&imm) {
            self.b(0x83);
            self.modrm_reg(ext, dst as u8);
            self.b(imm as i8 as u8);
        } else {
            self.b(0x81);
            self.modrm_reg(ext, dst as u8);
            self.d32(imm);
        }
    }
    fn alu_mi(&mut self, ext: u8, m: M, imm: i32, w: bool) {
        self.rex(w, 0, 0, m.0 as u8, false);
        if (-128..=127).contains(&imm) {
            self.b(0x83);
            self.modrm_mem(ext, m);
            self.b(imm as i8 as u8);
        } else {
            self.b(0x81);
            self.modrm_mem(ext, m);
            self.d32(imm);
        }
    }
    pub fn add_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x01, d, s)
    }
    pub fn sub_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x29, d, s)
    }
    pub fn and_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x21, d, s)
    }
    pub fn or_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x09, d, s)
    }
    pub fn xor_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x31, d, s)
    }
    pub fn cmp_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x39, d, s)
    }
    pub fn test_rr(&mut self, d: R, s: R) {
        self.alu_rr(0x85, d, s)
    }
    pub fn add_ri(&mut self, d: R, i: i32) {
        self.alu_ri(0, d, i)
    }
    pub fn sub_ri(&mut self, d: R, i: i32) {
        self.alu_ri(5, d, i)
    }
    pub fn and_ri(&mut self, d: R, i: i32) {
        self.alu_ri(4, d, i)
    }
    pub fn cmp_ri(&mut self, d: R, i: i32) {
        self.alu_ri(7, d, i)
    }
    /// cmp qword [m], imm32
    pub fn cmp_mi(&mut self, m: M, i: i32) {
        self.alu_mi(7, m, i, true)
    }
    /// cmp dword [m], imm32
    pub fn cmp32_mi(&mut self, m: M, i: i32) {
        self.alu_mi(7, m, i, false)
    }
    /// add qword [m], imm32
    pub fn add_mi(&mut self, m: M, i: i32) {
        self.alu_mi(0, m, i, true)
    }
    /// lock add qword [m], imm32
    pub fn lock_add_mi(&mut self, m: M, i: i32) {
        self.b(0xf0);
        self.alu_mi(0, m, i, true)
    }
    /// add dword [m], imm32
    pub fn add32_mi(&mut self, m: M, i: i32) {
        self.alu_mi(0, m, i, false)
    }
    /// cmp r64, qword [m]
    pub fn cmp_rm(&mut self, r: R, m: M) {
        self.rex(true, r as u8, 0, m.0 as u8, false);
        self.b(0x3b);
        self.modrm_mem(r as u8, m);
    }
    /// cmp r32, dword [m]
    pub fn cmp32_rm(&mut self, r: R, m: M) {
        self.rex(false, r as u8, 0, m.0 as u8, false);
        self.b(0x3b);
        self.modrm_mem(r as u8, m);
    }
    /// add r32, imm (32-bit op)
    pub fn add32_ri(&mut self, d: R, imm: i32) {
        self.rex(false, 0, 0, d as u8, false);
        self.b(0x81);
        self.modrm_reg(0, d as u8);
        self.d32(imm);
    }
    pub fn sub32_ri(&mut self, d: R, imm: i32) {
        self.rex(false, 0, 0, d as u8, false);
        self.b(0x81);
        self.modrm_reg(5, d as u8);
        self.d32(imm);
    }
    pub fn cmp32_ri(&mut self, d: R, imm: i32) {
        self.rex(false, 0, 0, d as u8, false);
        self.b(0x81);
        self.modrm_reg(7, d as u8);
        self.d32(imm);
    }
    pub fn imul_rr(&mut self, d: R, s: R) {
        self.rex(true, d as u8, 0, s as u8, false);
        self.b(0x0f);
        self.b(0xaf);
        self.modrm_reg(d as u8, s as u8);
    }
    pub fn neg(&mut self, d: R) {
        self.rex(true, 0, 0, d as u8, false);
        self.b(0xf7);
        self.modrm_reg(3, d as u8);
    }
    pub fn shl_ri(&mut self, d: R, n: u8) {
        self.rex(true, 0, 0, d as u8, false);
        self.b(0xc1);
        self.modrm_reg(4, d as u8);
        self.b(n);
    }
    pub fn sar_ri(&mut self, d: R, n: u8) {
        self.rex(true, 0, 0, d as u8, false);
        self.b(0xc1);
        self.modrm_reg(7, d as u8);
        self.b(n);
    }
    pub fn shr_ri(&mut self, d: R, n: u8) {
        self.rex(true, 0, 0, d as u8, false);
        self.b(0xc1);
        self.modrm_reg(5, d as u8);
        self.b(n);
    }
    /// movsxd r64, r32
    pub fn movsxd(&mut self, d: R, s: R) {
        self.rex(true, d as u8, 0, s as u8, false);
        self.b(0x63);
        self.modrm_reg(d as u8, s as u8);
    }
    /// setcc r8 + movzx r32, r8
    pub fn setcc(&mut self, cc: Cc, d: R) {
        self.rex(false, 0, 0, d as u8, (d as u8) >= 4);
        self.b(0x0f);
        self.b(0x90 | cc as u8);
        self.modrm_reg(0, d as u8);
        self.rex(false, d as u8, 0, d as u8, (d as u8) >= 4);
        self.b(0x0f);
        self.b(0xb6);
        self.modrm_reg(d as u8, d as u8);
    }
    /// cmovcc r64, r64
    pub fn cmov(&mut self, cc: Cc, d: R, s: R) {
        self.rex(true, d as u8, 0, s as u8, false);
        self.b(0x0f);
        self.b(0x40 | cc as u8);
        self.modrm_reg(d as u8, s as u8);
    }

    // ------------------------------------------------------------------ SSE2
    fn sse_rm(&mut self, prefix: u8, op: u8, x: X, m: M) {
        self.b(prefix);
        self.rex(false, x.0, 0, m.0 as u8, false);
        self.b(0x0f);
        self.b(op);
        self.modrm_mem(x.0, m);
    }
    fn sse_rr(&mut self, prefix: u8, op: u8, d: X, s: X) {
        self.b(prefix);
        self.rex(false, d.0, 0, s.0, false);
        self.b(0x0f);
        self.b(op);
        self.modrm_reg(d.0, s.0);
    }
    pub fn movsd_load(&mut self, x: X, m: M) {
        self.sse_rm(0xf2, 0x10, x, m)
    }
    pub fn movsd_store(&mut self, m: M, x: X) {
        self.sse_rm(0xf2, 0x11, x, m)
    }
    pub fn addsd(&mut self, d: X, s: X) {
        self.sse_rr(0xf2, 0x58, d, s)
    }
    pub fn subsd(&mut self, d: X, s: X) {
        self.sse_rr(0xf2, 0x5c, d, s)
    }
    pub fn mulsd(&mut self, d: X, s: X) {
        self.sse_rr(0xf2, 0x59, d, s)
    }
    pub fn divsd(&mut self, d: X, s: X) {
        self.sse_rr(0xf2, 0x5e, d, s)
    }
    /// ucomisd d, s (66 0F 2E)
    pub fn ucomisd(&mut self, d: X, s: X) {
        self.sse_rr(0x66, 0x2e, d, s)
    }
    /// cvtsi2sd x, r64
    pub fn cvtsi2sd(&mut self, d: X, s: R) {
        self.b(0xf2);
        self.rex(true, d.0, 0, s as u8, false);
        self.b(0x0f);
        self.b(0x2a);
        self.modrm_reg(d.0, s as u8);
    }
    /// xorpd x, x
    pub fn xorpd(&mut self, d: X, s: X) {
        self.sse_rr(0x66, 0x57, d, s)
    }

    // ------------------------------------------------------------------ control
    pub fn push(&mut self, r: R) {
        self.rex(false, 0, 0, r as u8, false);
        self.b(0x50 + (r as u8 & 7));
    }
    pub fn pop(&mut self, r: R) {
        self.rex(false, 0, 0, r as u8, false);
        self.b(0x58 + (r as u8 & 7));
    }
    pub fn ret(&mut self) {
        self.b(0xc3);
    }
    pub fn call_r(&mut self, r: R) {
        self.rex(false, 0, 0, r as u8, false);
        self.b(0xff);
        self.modrm_reg(2, r as u8);
    }
    pub fn jmp_r(&mut self, r: R) {
        self.rex(false, 0, 0, r as u8, false);
        self.b(0xff);
        self.modrm_reg(4, r as u8);
    }
    /// Call an absolute address (via rax).
    pub fn call_abs(&mut self, addr: usize) {
        self.mov_ri(Rax, addr as i64);
        self.call_r(Rax);
    }
    pub fn jmp(&mut self, l: Label) {
        if let Some(p) = self.labels[l.0 as usize] {
            let rel = p as isize - (self.pos() as isize + 2);
            if (-128..=127).contains(&rel) {
                self.b(0xeb);
                self.b(rel as i8 as u8);
                return;
            }
        }
        self.b(0xe9);
        self.fixups.push((self.pos(), l));
        self.d32(0);
    }
    pub fn jcc(&mut self, cc: Cc, l: Label) {
        if let Some(p) = self.labels[l.0 as usize] {
            let rel = p as isize - (self.pos() as isize + 2);
            if (-128..=127).contains(&rel) {
                self.b(0x70 | cc as u8);
                self.b(rel as i8 as u8);
                return;
            }
        }
        self.b(0x0f);
        self.b(0x80 | cc as u8);
        self.fixups.push((self.pos(), l));
        self.d32(0);
    }
    pub fn int3(&mut self) {
        self.b(0xcc);
    }
    pub fn ud2(&mut self) {
        self.b(0x0f);
        self.b(0x0b);
    }
    pub fn align(&mut self, n: usize) {
        while self.buf.len() % n != 0 {
            self.b(0x90);
        }
    }

    /// Resolve relative fixups. Returns Err if a label is unbound.
    pub fn resolve(&mut self) -> Result<(), String> {
        for &(at, l) in &self.fixups {
            let target = self.labels[l.0 as usize].ok_or_else(|| format!("unbound label {}", l.0))?;
            let rel = target as i64 - (at as i64 + 4);
            self.buf[at..at + 4].copy_from_slice(&(rel as i32).to_le_bytes());
        }
        Ok(())
    }

    /// Patch absolute label addresses for a code buffer that will live at `base`.
    pub fn patch_abs(&self, code: &mut [u8], base: usize) -> Result<(), String> {
        for &(at, l) in &self.abs_fixups {
            let target = self.labels[l.0 as usize].ok_or_else(|| format!("unbound label {}", l.0))?;
            code[at..at + 8].copy_from_slice(&((base + target) as u64).to_le_bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodings() {
        let mut a = Asm::new();
        a.mov_rr(Rax, Rbx);
        assert_eq!(a.buf, [0x48, 0x89, 0xd8]);
        a.buf.clear();
        a.load(Rax, M(R12, 8));
        assert_eq!(a.buf, [0x49, 0x8b, 0x44, 0x24, 0x08]);
        a.buf.clear();
        a.store(M(R13, 0), R9);
        assert_eq!(a.buf, [0x4d, 0x89, 0x4d, 0x00]);
        a.buf.clear();
        a.load_fs0(R15);
        assert_eq!(a.buf, [0x64, 0x4c, 0x8b, 0x3c, 0x25, 0, 0, 0, 0]);
        a.buf.clear();
        a.addsd(XMM0, XMM1);
        assert_eq!(a.buf, [0xf2, 0x0f, 0x58, 0xc1]);
        a.buf.clear();
        a.setcc(Cc::E, Rsi);
        assert_eq!(a.buf, [0x40, 0x0f, 0x94, 0xc6, 0x40, 0x0f, 0xb6, 0xf6]);
    }
}
