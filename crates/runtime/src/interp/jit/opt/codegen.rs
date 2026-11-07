//! Tier-2 x86-64 code generation from allocated IR.
//!
//! Register conventions are tier 1's (rbx frame, r12 slots, r13 thread state, r15
//! thread id), so tier-1 templates can be embedded and both tiers share the entry
//! trampoline/epilogue contract. Tier-2 code additionally owns a native frame of
//! `alloc.frame` bytes for spill slots (rsp-relative); the return-value out pointer
//! is at [rsp + frame].
//!
//! Scratch registers (never allocated): rax, r10, r11 and xmm0, xmm1, xmm15. The
//! shared routines (boxing, decref slow paths, polls) preserve every allocatable
//! register, so only Call/CallChecked/Template clobber allocated registers.

use super::build::{Built, Unit};
use super::ir::*;
use super::layout::*;
use super::regalloc::{Alloc, Loc};
use crate::ffi::*;
use crate::interp::jit::helpers as h;
use crate::interp::jit::tier1::{self, Gen};
use crate::interp::jit::x64::*;

const XS0: X = X(0);
const XS1: X = X(1);
const XS15: X = X(15);

pub struct Out {
    pub asm: Asm,
    /// (code unit pc, offset of the entry)
    pub entries: Vec<(u32, u32)>,
    pub sites: Vec<Box<super::Site>>,
}

struct Cg<'a, 'b> {
    g: Gen<'a>,
    f: &'b Func,
    al: &'b Alloc,
    nlp: i32,
    blabel: Vec<Label>,
    exit: Label,
    snap_label: Vec<Option<Label>>,
    rt_box_int: Label,
    rt_box_float: Label,
    rt_decref_shared: Label,
    rt_merge_zero: Label,
    deopt_counter: usize,
    /// descriptors of templates with stale unboxed locals (Frame::t2_site)
    sites: Vec<Box<super::Site>>,
    /// deoptimisations forced by frame introspection: (label, resume pc, stack depth)
    forced: Vec<(Label, u32, i32)>,
    /// Return with a frame object: (stub, back, snapshot of the dirty locals)
    ret_syncs: Vec<(Label, Label, SnapId)>,
    poll_back: Vec<(SnapId, Label)>,
    /// bytes pushed on the native stack since the spill area was set up (spill slot
    /// addresses are rsp-relative)
    sp_bias: i32,
}

fn fa<T: Copy>(f: T) -> i64 {
    debug_assert!(std::mem::size_of::<T>() == 8);
    unsafe { std::mem::transmute_copy::<T, usize>(&f) as i64 }
}

impl<'a, 'b> Cg<'a, 'b> {
    fn a(&mut self) -> &mut Asm {
        &mut self.g.a
    }
    fn loc(&self, v: V) -> Loc {
        self.al.loc[v as usize]
    }
    /// gpr holding v (loaded into `scratch` if spilled)
    fn gr(&mut self, v: V, scratch: R) -> R {
        match self.loc(v) {
            Loc::Reg(r) => r,
            Loc::Stack(o) => {
                let o = o + self.sp_bias;
                self.a().load(scratch, M(Rsp, o));
                scratch
            }
            _ => {
                // dead / undefined value (e.g. an unbound unboxed local): any bits do
                self.a().mov_ri(scratch, 0);
                scratch
            }
        }
    }
    /// xmm holding v
    fn xr(&mut self, v: V, scratch: X) -> X {
        match self.loc(v) {
            Loc::Xmm(x) => X(x),
            Loc::Stack(o) => {
                let o = o + self.sp_bias;
                self.a().movsd_load(scratch, M(Rsp, o));
                scratch
            }
            _ => {
                self.a().xorpd(scratch, scratch);
                scratch
            }
        }
    }
    /// register to compute v's value in (its own register, or `scratch`)
    fn dst(&self, v: V, scratch: R) -> R {
        match self.loc(v) {
            Loc::Reg(r) => r,
            _ => scratch,
        }
    }
    fn dstx(&self, v: V, scratch: X) -> X {
        match self.loc(v) {
            Loc::Xmm(x) => X(x),
            _ => scratch,
        }
    }
    /// store the value computed in r into v's home (if it is not r)
    fn put(&mut self, v: V, r: R) {
        match self.loc(v) {
            Loc::Reg(d) => {
                if d != r {
                    self.a().mov_rr(d, r)
                }
            }
            Loc::Stack(o) => {
                let o = o + self.sp_bias;
                self.a().store(M(Rsp, o), r)
            }
            _ => {}
        }
    }
    fn putx(&mut self, v: V, x: X) {
        match self.loc(v) {
            Loc::Xmm(d) => {
                if d != x.0 {
                    self.a().movsd_rr(X(d), x)
                }
            }
            Loc::Stack(o) => {
                let o = o + self.sp_bias;
                self.a().movsd_store(M(Rsp, o), x)
            }
            _ => {}
        }
    }
    fn base(&mut self, b: Base, scratch: R) -> R {
        match b {
            Base::Frame => Rbx,
            Base::Slots => R12,
            Base::Ts => R13,
            Base::V(v) => self.gr(v, scratch),
        }
    }
    /// operand: register or (fitting) immediate
    fn opnd(&mut self, o: Opnd, scratch: R) -> Result<R, i32> {
        match o {
            Opnd::V(v) => Ok(self.gr(v, scratch)),
            Opnd::I(i) if i >= i32::MIN as i64 && i <= i32::MAX as i64 => Err(i as i32),
            Opnd::I(i) => {
                self.a().mov_ri(scratch, i);
                Ok(scratch)
            }
        }
    }

    // ------------------------------------------------------------ refcounts
    /// inline incref of the object in r (r != r11)
    fn incref(&mut self, r: R) {
        let done = self.a().label();
        let slow = self.a().label();
        let a = self.a();
        a.load32(R11, M(r, OB_REF_LOCAL));
        a.add32_ri(R11, 1);
        a.jcc(Cc::E, done);
        a.cmp_rm(R15, M(r, OB_TID));
        a.jcc(Cc::NE, slow);
        a.store32(M(r, OB_REF_LOCAL), R11);
        a.bind(done);
        self.g.add_stub(Box::new(move |a: &mut Asm, _e, _n| {
            a.bind(slow);
            a.lock_add_mi(M(r, OB_REF_SHARED), 1 << REF_SHARED_SHIFT);
            a.jmp(done);
        }));
    }
    /// inline decref of the object in r (r != r11); slow paths preserve registers
    fn decref(&mut self, r: R) {
        let done = self.a().label();
        let shared = self.a().label();
        let zero = self.a().label();
        let a = self.a();
        a.load32(R11, M(r, OB_REF_LOCAL));
        a.cmp32_ri(R11, -1);
        a.jcc(Cc::E, done);
        a.cmp_rm(R15, M(r, OB_TID));
        a.jcc(Cc::NE, shared);
        a.sub32_ri(R11, 1);
        a.store32(M(r, OB_REF_LOCAL), R11);
        a.jcc(Cc::E, zero);
        a.bind(done);
        let (rs, rz) = (self.rt_decref_shared, self.rt_merge_zero);
        self.g.add_stub(Box::new(move |a: &mut Asm, _e, _n| {
            a.bind(shared);
            if r != Rax {
                a.push(Rax);
                a.push(Rax);
                a.mov_rr(Rax, r);
            }
            a.call_label(rs);
            if r != Rax {
                a.pop(Rax);
                a.pop(Rax);
            }
            a.jmp(done);
            a.bind(zero);
            if r != Rax {
                a.push(Rax);
                a.push(Rax);
                a.mov_rr(Rax, r);
            }
            a.call_label(rz);
            if r != Rax {
                a.pop(Rax);
                a.pop(Rax);
            }
            a.jmp(done);
        }));
    }

    // ------------------------------------------------------------ conditions
    /// Emit code jumping to `t` iff `c` holds.
    /// Emit the flags of an integer condition; returns its condition code (None for
    /// float compares and flag tests, which need branches).
    fn int_flags(&mut self, c: &Cond) -> Option<Cc> {
        match *c {
            Cond::Cmp(..) | Cond::Cmp32(..) | Cond::CmpMem(..) | Cond::CmpMem32(..) | Cond::Test(..) => {
                Some(self.cmp_only(c))
            }
            _ => None,
        }
    }

    fn jump_if(&mut self, c: &Cond, t: Label) {
        if let Some(cc) = self.int_flags(c) {
            self.a().jcc(cc, t);
            return;
        }
        self.jump_if_float(c, t)
    }

    /// Compare instruction(s) of an integer condition (flags set), its condition code.
    fn cmp_only(&mut self, c: &Cond) -> Cc {
        match *c {
            Cond::Cmp(a, b, cc) | Cond::Cmp32(a, b, cc) => {
                let w64 = matches!(c, Cond::Cmp(..));
                let ra = self.gr(a, R10);
                match self.opnd(b, R11) {
                    Ok(rb) => {
                        if w64 {
                            self.a().cmp_rr(ra, rb)
                        } else {
                            self.a().cmp32_rr(ra, rb)
                        }
                    }
                    Err(i) => {
                        if w64 {
                            self.a().cmp_ri(ra, i)
                        } else {
                            self.a().cmp32_ri(ra, i)
                        }
                    }
                }
                cc
            }
            Cond::CmpMem(bs, off, imm, cc) | Cond::CmpMem32(bs, off, imm, cc) => {
                let w64 = matches!(c, Cond::CmpMem(..));
                let rb = self.base(bs, R10);
                if imm >= i32::MIN as i64 && imm <= i32::MAX as i64 {
                    if w64 {
                        self.a().cmp_mi(M(rb, off), imm as i32)
                    } else {
                        self.a().cmp32_mi(M(rb, off), imm as i32)
                    }
                } else {
                    self.a().mov_ri(R11, imm);
                    if w64 {
                        self.a().cmp_mr(M(rb, off), R11)
                    } else {
                        self.a().cmp32_mr(M(rb, off), R11)
                    }
                }
                cc
            }
            Cond::Test(a, b, cc) => {
                let ra = self.gr(a, R10);
                match self.opnd(b, R11) {
                    Ok(rb) => self.a().test_rr(ra, rb),
                    Err(i) => self.a().test_ri(ra, i),
                }
                cc
            }
            _ => unreachable!("not an integer condition"),
        }
    }

    /// Branch on a float compare / flag test.
    fn jump_if_float(&mut self, c: &Cond, t: Label) {
        match *c {
            Cond::FCmp(a, b, cc) => {
                let xa = self.xr(a, XS0);
                let xb = self.xr(b, XS1);
                match cc {
                    Cc::L => {
                        self.a().ucomisd(xb, xa);
                        self.a().jcc(Cc::A, t);
                    }
                    Cc::LE => {
                        self.a().ucomisd(xb, xa);
                        self.a().jcc(Cc::AE, t);
                    }
                    Cc::G => {
                        self.a().ucomisd(xa, xb);
                        self.a().jcc(Cc::A, t);
                    }
                    Cc::GE => {
                        self.a().ucomisd(xa, xb);
                        self.a().jcc(Cc::AE, t);
                    }
                    Cc::E => {
                        let skip = self.a().label();
                        self.a().ucomisd(xa, xb);
                        self.a().jcc(Cc::P, skip);
                        self.a().jcc(Cc::E, t);
                        self.a().bind(skip);
                    }
                    _ => {
                        self.a().ucomisd(xa, xb);
                        self.a().jcc(Cc::P, t);
                        self.a().jcc(Cc::NE, t);
                    }
                }
            }
            Cond::FlagSet(addr) => {
                self.a().mov_ri(R10, addr as i64);
                self.a().cmp8_mi(M(R10, 0), 0);
                self.a().jcc(Cc::NE, t);
            }
            _ => unreachable!(),
        }
    }

    fn snap_target(&mut self, s: SnapId) -> Label {
        if let Some(l) = self.snap_label[s as usize] {
            return l;
        }
        let l = self.a().label();
        self.snap_label[s as usize] = Some(l);
        l
    }

    // ------------------------------------------------------------ instructions
    fn inst(&mut self, ins: &Inst) -> Result<(), String> {
        use Inst::*;
        match ins {
            Imm(d, x) => {
                let r = self.dst(*d, Rax);
                self.a().mov_ri(r, *x);
                self.put(*d, r);
            }
            FImm(d, x) => {
                let r = self.dstx(*d, XS0);
                if *x == 0.0 && x.is_sign_positive() {
                    self.a().xorpd(r, r);
                } else {
                    self.a().mov_ri(Rax, x.to_bits() as i64);
                    self.a().movq_xr(r, Rax);
                }
                self.putx(*d, r);
            }
            Mov(d, s) => match self.f.classes[*s as usize] {
                Class::G => {
                    let r = self.gr(*s, Rax);
                    self.put(*d, r);
                }
                Class::F => {
                    let x = self.xr(*s, XS0);
                    self.putx(*d, x);
                }
            },
            Load(d, b, off) | Load32(d, b, off) | Load8(d, b, off) => {
                let rb = self.base(*b, R10);
                let r = self.dst(*d, Rax);
                match ins {
                    Load(..) => self.a().load(r, M(rb, *off)),
                    Load32(..) => self.a().load32(r, M(rb, *off)),
                    _ => self.a().load8(r, M(rb, *off)),
                }
                self.put(*d, r);
            }
            Store(b, off, s) => {
                let rb = self.base(*b, R10);
                let rs = self.gr(*s, R11);
                self.a().store(M(rb, *off), rs);
            }
            StoreImm(b, off, x) => {
                let rb = self.base(*b, R10);
                self.a().store_i(M(rb, *off), *x);
            }
            Store32Imm(b, off, x) => {
                let rb = self.base(*b, R10);
                self.a().store32_i(M(rb, *off), *x);
            }
            LoadIdx(d, b, ix, disp) => {
                let rb = self.gr(*b, R10);
                let ri = self.gr(*ix, R11);
                let r = self.dst(*d, Rax);
                self.a().load_idx(r, rb, ri, 8, *disp);
                self.put(*d, r);
            }
            StoreIdx(b, ix, disp, s) => {
                let rb = self.gr(*b, R10);
                let ri = self.gr(*ix, R11);
                let rs = self.gr(*s, Rax);
                self.a().store_idx(rb, ri, 8, *disp, rs);
            }
            LoadF(d, b, off) => {
                let rb = self.base(*b, R10);
                let x = self.dstx(*d, XS0);
                self.a().movsd_load(x, M(rb, *off));
                self.putx(*d, x);
            }
            StoreF(b, off, s) => {
                let rb = self.base(*b, R10);
                let x = self.xr(*s, XS0);
                self.a().movsd_store(M(rb, *off), x);
            }
            Lea(d, b, off) => {
                let rb = self.base(*b, R10);
                let r = self.dst(*d, Rax);
                self.a().lea(r, M(rb, *off));
                self.put(*d, r);
            }
            SlotAddr(d, i) => {
                let r = self.dst(*d, Rax);
                let off = 8 * *i as i32;
                self.a().lea(r, M(R12, off));
                self.put(*d, r);
            }
            Alu(op, d, a, b) | AluOv(op, d, a, b, _) => {
                let ra = self.gr(*a, Rax);
                if matches!(op, super::ir::Alu::Shl | super::ir::Alu::Sar) {
                    // count in cl
                    self.a().mov_rr(Rax, ra);
                    let rb = self.opnd(*b, R11);
                    self.a().mov_rr(R10, Rcx);
                    match rb {
                        Ok(r) => self.a().mov_rr(Rcx, r),
                        Err(i) => self.a().mov_ri(Rcx, i as i64),
                    }
                    if *op == super::ir::Alu::Shl {
                        self.a().shl_cl(Rax)
                    } else {
                        self.a().sar_cl(Rax)
                    }
                    self.a().mov_rr(Rcx, R10);
                    self.put(*d, Rax);
                    return Ok(());
                }
                self.a().mov_rr(Rax, ra);
                let rb = self.opnd(*b, R11);
                {
                    use super::ir::Alu as A;
                    let asm = &mut self.g.a;
                    match (op, rb) {
                        (A::Add, Ok(r)) => asm.add_rr(Rax, r),
                        (A::Add, Err(i)) => asm.add_ri(Rax, i),
                        (A::Sub, Ok(r)) => asm.sub_rr(Rax, r),
                        (A::Sub, Err(i)) => asm.sub_ri(Rax, i),
                        (A::Mul, Ok(r)) => asm.imul_rr(Rax, r),
                        (A::Mul, Err(i)) => asm.imul_rri(Rax, Rax, i),
                        (A::And, Ok(r)) => asm.and_rr(Rax, r),
                        (A::And, Err(i)) => asm.and_ri(Rax, i),
                        (A::Or, Ok(r)) => asm.or_rr(Rax, r),
                        (A::Or, Err(i)) => asm.or_ri(Rax, i),
                        (A::Xor, Ok(r)) => asm.xor_rr(Rax, r),
                        (A::Xor, Err(i)) => asm.xor_ri(Rax, i),
                        _ => unreachable!(),
                    }
                }
                if let AluOv(_, _, _, _, s) = ins {
                    let l = self.snap_target(*s);
                    self.a().jcc(Cc::O, l);
                }
                self.put(*d, Rax);
            }
            Neg(d, s) | NegOv(d, s, _) => {
                let r = self.gr(*s, Rax);
                self.a().mov_rr(Rax, r);
                self.a().neg(Rax);
                if let NegOv(_, _, sn) = ins {
                    let l = self.snap_target(*sn);
                    self.a().jcc(Cc::O, l);
                }
                self.put(*d, Rax);
            }
            IDivMod(d, a, b, floor) => {
                // rax/rdx are needed by idiv: rdx may be allocated -> save it in r11
                let ra = self.gr(*a, Rax);
                self.a().mov_rr(Rax, ra);
                let rb = self.gr(*b, R10);
                if rb != R10 {
                    self.a().mov_rr(R10, rb);
                }
                self.a().mov_rr(R11, Rdx);
                self.a().cqo();
                self.a().idiv(R10);
                // Python semantics: adjust when the remainder is non-zero and has the
                // other sign than the divisor
                let done = self.a().label();
                self.a().test_rr(Rdx, Rdx);
                self.a().jcc(Cc::E, done);
                // sign(rdx) != sign(r10)  <=>  (rdx ^ r10) < 0
                self.a().push(Rdx);
                self.a().xor_rr(Rdx, R10);
                self.a().pop(Rdx);
                // (push/pop do not change flags)
                self.a().jcc(Cc::NS, done);
                if *floor {
                    self.a().add_ri(Rax, -1);
                } else {
                    self.a().add_rr(Rdx, R10);
                }
                self.a().bind(done);
                if !*floor {
                    self.a().mov_rr(Rax, Rdx);
                }
                self.a().mov_rr(Rdx, R11);
                self.put(*d, Rax);
            }
            FAlu(op, d, a, b) => {
                let xa = self.xr(*a, XS0);
                self.a().movsd_rr(XS15, xa);
                let xb = self.xr(*b, XS1);
                {
                    let asm = &mut self.g.a;
                    match op {
                        super::ir::FAlu::Add => asm.addsd(XS15, xb),
                        super::ir::FAlu::Sub => asm.subsd(XS15, xb),
                        super::ir::FAlu::Mul => asm.mulsd(XS15, xb),
                        super::ir::FAlu::Div => asm.divsd(XS15, xb),
                    }
                }
                self.putx(*d, XS15);
            }
            FNeg(d, s) => {
                let x = self.xr(*s, XS0);
                self.a().movq_rx(Rax, x);
                self.a().mov_ri(R10, i64::MIN);
                self.a().xor_rr(Rax, R10);
                self.a().movq_xr(XS15, Rax);
                self.putx(*d, XS15);
            }
            FSqrt(d, s) => {
                let x = self.xr(*s, XS0);
                self.a().sqrtsd(XS15, x);
                self.putx(*d, XS15);
            }
            I2F(d, s) => {
                let r = self.gr(*s, Rax);
                self.a().cvtsi2sd(XS15, r);
                self.putx(*d, XS15);
            }
            SetCond(d, c) => {
                let r = self.dst(*d, Rax);
                match c {
                    Cond::FCmp(..) | Cond::FlagSet(_) => {
                        let yes = self.a().label();
                        let done = self.a().label();
                        self.jump_if(c, yes);
                        self.a().mov_ri(r, 0);
                        self.a().jmp(done);
                        self.a().bind(yes);
                        self.a().mov_ri(r, 1);
                        self.a().bind(done);
                    }
                    _ => {
                        // flags from the compare, then setcc + zero extension (r may be
                        // an operand: setcc writes after the compare)
                        let cc = self.cmp_only(c);
                        self.a().setcc(cc, r);
                    }
                }
                self.put(*d, r);
            }
            Select(d, c, a, b) => {
                let yes = self.a().label();
                let done = self.a().label();
                self.jump_if(c, yes);
                let rb = self.gr(*b, Rax);
                self.a().mov_rr(Rax, rb);
                self.a().jmp(done);
                self.a().bind(yes);
                let ra = self.gr(*a, Rax);
                self.a().mov_rr(Rax, ra);
                self.a().bind(done);
                self.put(*d, Rax);
            }
            Guard(c, s) => {
                let l = self.snap_target(*s);
                self.jump_if(c, l);
            }
            Incref(v) => {
                let r = self.gr(*v, Rax);
                self.incref(r);
            }
            Decref(v) => {
                let r = self.gr(*v, Rax);
                self.decref(r);
            }
            XDecref(v) => {
                let r = self.gr(*v, Rax);
                let skip = self.a().label();
                self.a().test_rr(r, r);
                self.a().jcc(Cc::E, skip);
                self.decref(r);
                self.a().bind(skip);
            }
            Box(k, d, s) => {
                match k {
                    BoxK::Int => {
                        let r = self.gr(*s, Rax);
                        self.a().mov_rr(Rax, r);
                        let l = self.rt_box_int;
                        self.a().call_label(l);
                    }
                    BoxK::Float => {
                        let x = self.xr(*s, XS0);
                        if x != XS0 {
                            self.a().movsd_rr(XS0, x);
                        }
                        let l = self.rt_box_float;
                        self.a().call_label(l);
                    }
                    BoxK::Bool => {
                        let r = self.gr(*s, R10);
                        self.a().mov_ri(Rax, crate::types::bool_::false_obj().as_ptr() as i64);
                        self.a().mov_ri(R11, crate::types::bool_::true_obj().as_ptr() as i64);
                        self.a().test_rr(r, r);
                        self.a().cmov(Cc::NE, Rax, R11);
                    }
                }
                self.put(*d, Rax);
            }
            UnboxInt(d, s, sn) => {
                let l = self.snap_target(*sn);
                let p = self.gr(*s, R10);
                let a = &mut self.g.a;
                a.mov_ri(R11, crate::types::int::int_type() as i64);
                a.cmp_rm(R11, M(p, OB_TYPE));
                a.jcc(Cc::NE, l);
                a.load(R11, M(p, LV_TAG));
                a.cmp_ri(R11, 3 << 3);
                a.jcc(Cc::AE, l);
                // value = d0 | d1 << 30 (d1 only if 2 digits)
                a.load32(Rax, M(p, LV_DIGIT));
                let one = a.label();
                a.cmp_ri(R11, 2 << 3);
                a.jcc(Cc::B, one);
                a.push(Rcx);
                a.load32(Rcx, M(p, LV_DIGIT + 4));
                a.shl_ri(Rcx, 30);
                a.or_rr(Rax, Rcx);
                a.pop(Rcx);
                a.bind(one);
                // sign: tag & 3 == 2 -> negative
                let pos = a.label();
                a.and_ri(R11, 2);
                a.jcc(Cc::E, pos);
                a.neg(Rax);
                a.bind(pos);
                self.put(*d, Rax);
            }
            UnboxFloat(d, s, sn) => {
                let l = self.snap_target(*sn);
                let p = self.gr(*s, R10);
                let x = self.dstx(*d, XS15);
                let a = &mut self.g.a;
                a.mov_ri(R11, crate::types::float::float_type() as i64);
                a.cmp_rm(R11, M(p, OB_TYPE));
                a.jcc(Cc::NE, l);
                a.movsd_load(x, M(p, FVAL));
                self.putx(*d, x);
            }
            UnboxNumF(d, s, sn) => {
                let l = self.snap_target(*sn);
                let p = self.gr(*s, R10);
                let a = &mut self.g.a;
                let not_f = a.label();
                let done = a.label();
                a.load(R11, M(p, OB_TYPE));
                a.mov_ri(Rax, crate::types::float::float_type() as i64);
                a.cmp_rr(R11, Rax);
                a.jcc(Cc::NE, not_f);
                a.movsd_load(XS15, M(p, FVAL));
                a.jmp(done);
                a.bind(not_f);
                a.mov_ri(Rax, crate::types::int::int_type() as i64);
                a.cmp_rr(R11, Rax);
                a.jcc(Cc::NE, l);
                a.load(R11, M(p, LV_TAG));
                a.cmp_ri(R11, 16);
                a.jcc(Cc::AE, l);
                a.load32(Rax, M(p, LV_DIGIT));
                let pos = a.label();
                a.and_ri(R11, 2);
                a.jcc(Cc::E, pos);
                a.neg(Rax);
                a.bind(pos);
                a.cvtsi2sd(XS15, Rax);
                a.bind(done);
                self.putx(*d, XS15);
            }
            LoadLocalUnboxed { home, slot, k, snap } => {
                let l = self.snap_target(*snap);
                let skip = self.a().label();
                let off = 8 * *slot as i32;
                self.a().load(R10, M(R12, off));
                self.a().test_rr(R10, R10);
                self.a().jcc(Cc::E, skip);
                match k {
                    BoxK::Float => {
                        let a = &mut self.g.a;
                        a.mov_ri(R11, crate::types::float::float_type() as i64);
                        a.cmp_rm(R11, M(R10, OB_TYPE));
                        a.jcc(Cc::NE, l);
                        a.movsd_load(XS15, M(R10, FVAL));
                        self.putx(*home, XS15);
                    }
                    _ => {
                        let a = &mut self.g.a;
                        a.mov_ri(R11, crate::types::int::int_type() as i64);
                        a.cmp_rm(R11, M(R10, OB_TYPE));
                        a.jcc(Cc::NE, l);
                        a.load(R11, M(R10, LV_TAG));
                        a.cmp_ri(R11, 16);
                        a.jcc(Cc::AE, l);
                        a.load32(Rax, M(R10, LV_DIGIT));
                        let pos = a.label();
                        a.and_ri(R11, 2);
                        a.jcc(Cc::E, pos);
                        a.neg(Rax);
                        a.bind(pos);
                        self.put(*home, Rax);
                    }
                }
                self.a().bind(skip);
            }
            TryLock(v, sn) => {
                let l = self.snap_target(*sn);
                let r = self.gr(*v, R10);
                let a = &mut self.g.a;
                a.mov_ri(Rax, 0);
                a.mov_ri(R11, 1);
                a.lock_cmpxchg8(M(r, OB_MUTEX), R11);
                a.jcc(Cc::NE, l);
            }
            Unlock(v) => {
                let r = self.gr(*v, R10);
                self.a().store8_i(M(r, OB_MUTEX), 0);
            }
            Call { addr, args, fargs, ret, fret } => {
                const AREGS: [R; 6] = [Rdi, Rsi, Rdx, Rcx, R8, R9];
                if args.len() > 6 || fargs.len() > 2 {
                    return Err("tier2: too many call arguments".into());
                }
                // evaluate into the stack area below rsp? use push-free staging:
                // load operands in reverse dependency order through rax
                let mut staged: Vec<(R, Opnd)> = args.iter().enumerate().map(|(k, o)| (AREGS[k], *o)).collect();
                // arguments may live in argument registers: stage all through the
                // native stack (red-zone free: push/pop pairs)
                for (_, o) in staged.iter() {
                    match *o {
                        Opnd::V(v) => {
                            let r = self.gr(v, Rax);
                            self.a().push(r);
                            self.sp_bias += 8;
                        }
                        Opnd::I(i) => {
                            self.a().mov_ri(Rax, i);
                            self.a().push(Rax);
                            self.sp_bias += 8;
                        }
                    }
                }
                staged.reverse();
                for (r, _) in staged.iter() {
                    self.a().pop(*r);
                }
                self.sp_bias = 0;
                // fargs: move into xmm0/xmm1 (they are scratch)
                if fargs.len() == 2 {
                    let x1 = self.xr(fargs[1], XS15);
                    let x0 = self.xr(fargs[0], XS0);
                    if x0 != XS0 {
                        self.a().movsd_rr(XS0, x0);
                    }
                    if x1 != XS1 {
                        self.a().movsd_rr(XS1, x1);
                    }
                } else if fargs.len() == 1 {
                    let x0 = self.xr(fargs[0], XS0);
                    if x0 != XS0 {
                        self.a().movsd_rr(XS0, x0);
                    }
                }
                self.a().call_abs(*addr);
                if let Some(r) = ret {
                    self.put(*r, Rax);
                }
                if let Some(r) = fret {
                    self.putx(*r, XS0);
                }
            }
            CallChecked { addr, args, err } => {
                const AREGS: [R; 6] = [Rdi, Rsi, Rdx, Rcx, R8, R9];
                for o in args.iter() {
                    match *o {
                        Opnd::V(v) => {
                            let r = self.gr(v, Rax);
                            self.a().push(r);
                            self.sp_bias += 8;
                        }
                        Opnd::I(i) => {
                            self.a().mov_ri(Rax, i);
                            self.a().push(Rax);
                            self.sp_bias += 8;
                        }
                    }
                }
                for k in (0..args.len()).rev() {
                    self.a().pop(AREGS[k]);
                }
                self.sp_bias = 0;
                self.a().call_abs(*addr);
                let l = self.snap_target(*err);
                self.a().test_rr(Rax, Rax);
                self.a().jcc(Cc::NE, l);
            }
            Template(i, exit_snap) => {
                let i = *i as usize;
                let saved = self.g.exit;
                // with stale unboxed locals: publish where they live for the duration
                // of the template (see opt::sync_frame); a jump out of the template
                // goes through a trampoline doing the same check as the fall-through
                let mut tramp: Option<(usize, Label)> = None;
                if let Some(s) = exit_snap {
                    let l = self.snap_target(*s);
                    self.g.exit = l;
                    let site = self.site_of(*s);
                    let p = &*site as *const super::Site as i64;
                    self.sites.push(site);
                    self.a().mov_ri(R10, p);
                    self.a().store(M(Rbx, T2_SITE), R10);
                    let ins = self.g.an.instrs[i];
                    if ins.op.is_jump() {
                        let t = self.g.an.index_of[ins.arg as usize] as usize;
                        let tl = self.a().label();
                        let orig = std::mem::replace(&mut self.g.ilabel[t], tl);
                        tramp = Some((t, orig));
                    }
                }
                let r = self.g.emit(i);
                self.g.exit = saved;
                r?;
                if exit_snap.is_some() {
                    if let Some(n) = self.g.an.instrs.get(i + 1).copied() {
                        let depth = self.g.an.depth[i + 1];
                        self.after_site(n.start, depth);
                    }
                    if let Some((t, orig)) = tramp {
                        let tl = std::mem::replace(&mut self.g.ilabel[t], orig);
                        let skip = self.a().label();
                        self.a().jmp(skip);
                        self.a().bind(tl);
                        let tgt = self.g.an.instrs[t];
                        let depth = self.g.an.depth[t];
                        self.after_site(tgt.start, depth);
                        self.a().jmp(orig);
                        self.a().bind(skip);
                    }
                }
            }
            Poll(s) => {
                let l = self.snap_target(*s);
                let back = self.a().label();
                self.a().load(Rax, M(R13, TS_EVAL_BREAKER));
                self.a().test_rr(Rax, Rax);
                self.a().jcc(Cc::NE, l);
                self.a().bind(back);
                // the poll stub returns here
                self.poll_back.push((*s, back));
            }
            SetLasti(pc) => {
                self.a().store32_i(M(Rbx, LASTI), *pc as i32);
            }
            MarkDefined(slot) => {
                let skip = self.a().label();
                let off = 8 * *slot as i32;
                self.a().cmp_mi(M(R12, off), 0);
                self.a().jcc(Cc::NE, skip);
                self.a().mov_ri(R10, crate::types::none::none().as_ptr() as i64);
                self.a().store(M(R12, off), R10);
                self.a().bind(skip);
            }
            SyncLocal { slot, v, k, maybe } => {
                self.sync_local(*slot, *v, *k, *maybe);
            }
        }
        Ok(())
    }

    /// Box the unboxed local value v and store it into frame slot `slot` (releasing
    /// the previous object); skipped at run time when `maybe` and the slot is NULL.
    fn sync_local(&mut self, slot: u32, v: V, k: BoxK, maybe: bool) {
        let skip = self.a().label();
        let off = 8 * slot as i32;
        if maybe {
            self.a().cmp_mi(M(R12, off), 0);
            self.a().jcc(Cc::E, skip);
        }
        self.box_into_rax(v, k);
        self.a().load(R10, M(R12, off));
        self.a().store(M(R12, off), Rax);
        self.a().mov_rr(Rax, R10);
        let s2 = self.a().label();
        self.a().test_rr(Rax, Rax);
        self.a().jcc(Cc::E, s2);
        self.decref(Rax);
        self.a().bind(s2);
        self.a().bind(skip);
    }
    fn box_into_rax(&mut self, v: V, k: BoxK) {
        match k {
            BoxK::Int => {
                let r = self.gr(v, Rax);
                self.a().mov_rr(Rax, r);
                let l = self.rt_box_int;
                self.a().call_label(l);
            }
            BoxK::Float => {
                let x = self.xr(v, XS0);
                if x != XS0 {
                    self.a().movsd_rr(XS0, x);
                }
                let l = self.rt_box_float;
                self.a().call_label(l);
            }
            BoxK::Bool => {
                let r = self.gr(v, R10);
                self.a().mov_ri(Rax, crate::types::bool_::false_obj().as_ptr() as i64);
                self.a().mov_ri(R11, crate::types::bool_::true_obj().as_ptr() as i64);
                self.a().test_rr(r, r);
                self.a().cmov(Cc::NE, Rax, R11);
            }
        }
    }
    /// Materialise one snapshot entry into frame slot `slot` (absolute slot index).
    fn mat_into(&mut self, m: &Mat, slot: i32) {
        let off = 8 * slot;
        match *m {
            Mat::InFrame => {}
            Mat::Owned(v) => {
                let r = self.gr(v, Rax);
                self.a().store(M(R12, off), r);
            }
            Mat::Borrowed(v) => {
                let r = self.gr(v, Rax);
                if r != Rax {
                    self.a().mov_rr(Rax, r);
                }
                self.incref(Rax);
                self.a().store(M(R12, off), Rax);
            }
            Mat::Const(p) => {
                self.a().mov_ri(Rax, p as i64);
                if p != 0 && unsafe { !(*(p as *mut PyObject)).is_immortal() } {
                    self.incref(Rax);
                }
                self.a().store(M(R12, off), Rax);
            }
            Mat::BoxInt(v) => {
                self.box_into_rax(v, BoxK::Int);
                self.a().store(M(R12, off), Rax);
            }
            Mat::BoxFloat(v) => {
                self.box_into_rax(v, BoxK::Float);
                self.a().store(M(R12, off), Rax);
            }
            Mat::BoxBool(v) => {
                self.box_into_rax(v, BoxK::Bool);
                self.a().store(M(R12, off), Rax);
            }
        }
    }
    /// An owned reference for snapshot entry `m` in rax (0 for none).
    fn mat_rax(&mut self, m: &Mat) {
        match *m {
            Mat::InFrame => self.a().mov_ri(Rax, 0),
            Mat::Owned(v) => {
                let r = self.gr(v, Rax);
                if r != Rax {
                    self.a().mov_rr(Rax, r);
                }
            }
            Mat::Borrowed(v) => {
                let r = self.gr(v, Rax);
                if r != Rax {
                    self.a().mov_rr(Rax, r);
                }
                self.incref(Rax);
            }
            Mat::Const(p) => {
                self.a().mov_ri(Rax, p as i64);
                if p != 0 && unsafe { !(*(p as *mut PyObject)).is_immortal() } {
                    self.incref(Rax);
                }
            }
            Mat::BoxInt(v) => self.box_into_rax(v, BoxK::Int),
            Mat::BoxFloat(v) => self.box_into_rax(v, BoxK::Float),
            Mat::BoxBool(v) => self.box_into_rax(v, BoxK::Bool),
        }
    }

    /// Stub of an inline deoptimisation: rebuild the callee frame, let the
    /// interpreter finish it, continue at the call's continuation with its result.
    fn inline_stub(&mut self, s: &Snapshot) {
        let inl = s.inl.expect("inline snapshot");
        save_all(self.a());
        self.sp_bias = 192;
        let mut items: Vec<Mat> = s.locals.iter().map(|&(_, m, _)| m).collect();
        let nloc = items.len() as i64;
        items.extend(s.stack.iter().copied());
        let count = items.len() as i32;
        let pad = count % 2 == 1;
        if pad {
            self.a().sub_ri(Rsp, 8);
            self.sp_bias += 8;
        }
        for m in items.iter().rev() {
            self.mat_rax(m);
            self.a().push(Rax);
            self.sp_bias += 8;
        }
        {
            let a = self.a();
            a.mov_ri(Rdi, inl.func as i64);
            a.mov_rr(Rsi, Rsp);
            a.mov_ri(Rdx, nloc | ((s.stack.len() as i64) << 32));
            a.mov_ri(Rcx, s.resume as i64);
            a.mov_ri(R8, s.lasti as i64);
            a.mov_ri(R9, (s.kind == SnapKind::InlineError) as i64);
            a.call_abs(fa(crate::interp::jit::h2_run_inlined as unsafe extern "C" fn(_, _, _, _, _, _) -> _) as usize);
            a.add_ri(Rsp, 8 * count + if pad { 8 } else { 0 });
        }
        self.sp_bias = 0;
        restore_all(self.a());
        let err = self.snap_target(inl.err);
        self.a().test_rr(Rax, Rax);
        self.a().jcc(Cc::E, err);
        self.put(inl.res, Rax);
        let c = self.blabel[inl.cont as usize];
        self.a().jmp(c);
    }

    /// Site descriptor for the dirty locals of a template exit snapshot.
    fn site_of(&self, s: SnapId) -> Box<super::Site> {
        let snap = &self.f.snaps[s as usize];
        let entries = snap
            .locals
            .iter()
            .filter_map(|&(slot, m, maybe)| {
                let (v, float) = match m {
                    Mat::BoxInt(v) => (v, false),
                    Mat::BoxFloat(v) => (v, true),
                    _ => return None,
                };
                let off = match self.loc(v) {
                    Loc::Stack(o) => o,
                    Loc::None => -1,
                    // regalloc keeps these in spill slots across templates
                    l => panic!("tier2: site value v{} in {:?}", v, l),
                };
                Some(super::SiteEntry { slot, float, maybe, off })
            })
            .collect();
        Box::new(super::Site { entries })
    }

    /// After a template with a site: clear it; if frame introspection refreshed the
    /// frame slots meanwhile, continue in the interpreter at `resume`.
    fn after_site(&mut self, resume: u32, depth: i32) {
        if depth < 0 {
            return;
        }
        let forced = self.a().label();
        self.forced.push((forced, resume, depth));
        let a = self.a();
        a.load(R10, M(Rbx, T2_SITE));
        a.store_i(M(Rbx, T2_SITE), 0);
        a.cmp_ri(R10, 1);
        a.jcc(Cc::E, forced);
    }

    fn emit_forced_and_ret_syncs(&mut self) {
        let nlp = self.nlp;
        for (l, resume, depth) in std::mem::take(&mut self.forced) {
            let e = self.exit;
            let a = self.a();
            a.bind(l);
            a.store32_i(M(Rbx, NEXT_INSTR), resume as i32);
            a.store_i(M(Rbx, STACK_TOP), nlp + depth);
            a.mov_ri(Rax, tier1::ST_DEOPT as i64);
            a.jmp(e);
        }
        for (stub, back, s) in std::mem::take(&mut self.ret_syncs) {
            self.a().bind(stub);
            let snap = self.f.snaps[s as usize].clone();
            self.sync_snapshot_locals(&snap);
            self.a().jmp(back);
        }
    }

    fn sync_snapshot_locals(&mut self, s: &Snapshot) {
        for &(slot, m, maybe) in &s.locals {
            let (v, k) = match m {
                Mat::BoxInt(v) => (v, BoxK::Int),
                Mat::BoxFloat(v) => (v, BoxK::Float),
                _ => continue,
            };
            self.sync_local(slot, v, k, maybe);
        }
    }

    /// Emit the stub of snapshot s (bound at its label).
    fn snapshot_stub(&mut self, sid: usize) {
        let l = match self.snap_label[sid] {
            Some(l) => l,
            None => return,
        };
        let s = self.f.snaps[sid].clone();
        self.a().bind(l);
        match s.kind {
            SnapKind::InlineDeopt | SnapKind::InlineError => self.inline_stub(&s),
            SnapKind::TemplateExit => {
                // status in eax must survive the sync
                // (frame slots already refreshed by frame introspection: keep them)
                let skip = self.a().label();
                self.a().cmp_mi(M(Rbx, T2_SITE), 1);
                self.a().jcc(Cc::E, skip);
                self.a().push(Rax);
                self.a().push(Rax);
                self.sp_bias = 16;
                self.sync_snapshot_locals(&s);
                self.sp_bias = 0;
                self.a().pop(Rax);
                self.a().pop(Rax);
                self.a().bind(skip);
                let e = self.exit;
                self.a().jmp(e);
            }
            SnapKind::Deopt => {
                for (k, m) in s.stack.iter().enumerate() {
                    self.mat_into(m, self.nlp + k as i32);
                }
                self.sync_snapshot_locals(&s);
                let nlp = self.nlp;
                let dc = self.deopt_counter as i64;
                let a = self.a();
                a.mov_ri(R10, dc);
                a.add32_mi(M(R10, 0), 1);
                a.store32_i(M(Rbx, NEXT_INSTR), s.resume as i32);
                a.store_i(M(Rbx, STACK_TOP), nlp + s.stack.len() as i32);
                a.mov_ri(Rax, tier1::ST_DEOPT as i64);
                let e = self.exit;
                self.a().jmp(e);
            }
            SnapKind::Error => {
                for (k, m) in s.stack.iter().enumerate() {
                    self.mat_into(m, self.nlp + k as i32);
                }
                self.sync_snapshot_locals(&s);
                let nlp = self.nlp;
                let a = self.a();
                a.store32_i(M(Rbx, LASTI), s.lasti as i32);
                a.store_i(M(Rbx, STACK_TOP), nlp + s.stack.len() as i32);
                a.mov_ri(Rax, tier1::ST_ERROR as i64);
                let e = self.exit;
                self.a().jmp(e);
            }
            SnapKind::Poll => {
                // stack is materialised at polls; sync locals so a handler (signal,
                // exception) sees current values, then run the eval breaker
                for (k, m) in s.stack.iter().enumerate() {
                    if *m != Mat::InFrame {
                        self.mat_into(m, self.nlp + k as i32);
                    }
                }
                self.sync_snapshot_locals(&s);
                let nlp = self.nlp;
                self.a().store32_i(M(Rbx, LASTI), s.lasti as i32);
                save_all(self.a());
                self.a().mov_rr(Rdi, R13);
                self.a().call_abs(fa(h::h_eval_breaker as unsafe extern "C" fn(_) -> _) as usize);
                restore_all(self.a());
                let err = self.a().label();
                self.a().test_rr(Rax, Rax);
                self.a().jcc(Cc::NE, err);
                let back = self.poll_back.iter().find(|(x, _)| *x as usize == sid).map(|x| x.1);
                if let Some(b) = back {
                    self.a().jmp(b);
                }
                self.a().bind(err);
                let a = self.a();
                a.store_i(M(Rbx, STACK_TOP), nlp + s.stack.len() as i32);
                a.mov_ri(Rax, tier1::ST_ERROR as i64);
                let e = self.exit;
                self.a().jmp(e);
            }
        }
    }
}


/// Push all caller-saved gprs except rax (8) and spill xmm0..15: 200 bytes, keeping
/// 16-byte alignment when entered with rsp % 16 == 0 or 8 as documented by callers.
fn save_all(a: &mut Asm) {
    // keeps rsp % 16 unchanged + 8 regs*8 = 64, + 136 = 200 -> callers adjust
    for r in [Rcx, Rdx, Rsi, Rdi, R8, R9, R10, R11] {
        a.push(r);
    }
    a.sub_ri(Rsp, 128);
    for k in 0..16u8 {
        a.movsd_store(M(Rsp, 8 * k as i32), X(k));
    }
}
fn restore_all(a: &mut Asm) {
    for k in 0..16u8 {
        a.movsd_load(X(k), M(Rsp, 8 * k as i32));
    }
    a.add_ri(Rsp, 128);
    for r in [R11, R10, R9, R8, Rdi, Rsi, Rdx, Rcx] {
        a.pop(r);
    }
}

/// (jit6) One process-wide copy of the routines below; units bind their labels to a
/// `jmp` there (r11 is free at every entry, see the register contract).
fn emit_routines(a: &mut Asm, box_int: Label, box_float: Label, dshared: Label, mzero: Label) {
    static SHARED: std::sync::OnceLock<Option<[usize; 4]>> = std::sync::OnceLock::new();
    let shared = SHARED.get_or_init(|| {
        let mut b = Asm::new();
        let l = [b.label(), b.label(), b.label(), b.label()];
        emit_routines_body(&mut b, l[0], l[1], l[2], l[3]);
        b.resolve().ok()?;
        let base = crate::interp::jit::mem::install(&b.buf, |_, _| {})?;
        Some([base + b.label_pos(l[0])?, base + b.label_pos(l[1])?, base + b.label_pos(l[2])?, base + b.label_pos(l[3])?])
    });
    match shared {
        Some(s) => {
            for (l, &t) in [box_int, box_float, dshared, mzero].into_iter().zip(s.iter()) {
                a.bind(l);
                a.mov_ri(R11, t as i64);
                a.jmp_r(R11);
            }
        }
        None => emit_routines_body(a, box_int, box_float, dshared, mzero),
    }
}

/// Shared tier-2 routines (all preserve every register except rax, r10, r11):
/// * box_int:  rax = i64 -> rax = int object
/// * box_float: xmm0 = f64 -> rax = float object
/// * decref_shared / merge_zero: rax = object
fn emit_routines_body(a: &mut Asm, box_int: Label, box_float: Label, dshared: Label, mzero: Label) {
    let tl = crate::tlab::jit_layout();
    let tls = tl.tls_offset as i32;
    let heads = tl.heads as i32;
    let counts = tl.counts as i32;
    let int_t = crate::types::int::int_type();
    let float_t = crate::types::float::float_type();
    let int_size = unsafe { ((*int_t).tp_basicsize as usize).max(LV_DIGIT as usize + 4) };
    let ci = crate::tlab::class_index(int_size) as i32;
    let cf = crate::tlab::class_index(std::mem::size_of::<PyFloatObject>()) as i32;
    let small = crate::interp::specialize::small_int_table() as i64;

    // Calls a Rust function preserving all registers; entered with rsp % 16 == 8
    // (just after a `call` into the routine). Argument already in rdi / xmm0.
    let preserving_call = |a: &mut Asm, f: i64| {
        save_all(a); // 8 pushes (64) + 128 = 192 -> rsp % 16 == 8 still
        a.sub_ri(Rsp, 8);
        a.call_abs(f as usize);
        a.add_ri(Rsp, 8);
        restore_all(a);
    };
    let hdr = |a: &mut Asm, tp: i64| {
        a.store(M(Rax, OB_TID), R15);
        a.mov_ri(R10, 1i64 << 32);
        a.store(M(Rax, 8), R10);
        a.store_i(M(Rax, OB_REF_SHARED), 0);
        a.mov_ri(R10, tp);
        a.store(M(Rax, OB_TYPE), R10);
    };

    // ---- box_int
    a.bind(box_int);
    {
        let big = a.label();
        let slow = a.label();
        let slow2 = a.label();
        a.lea(R10, M(Rax, 5));
        a.cmp_ri(R10, 1030);
        a.jcc(Cc::A, big);
        a.mov_ri(R11, small);
        a.load_idx(Rax, R11, R10, 8, 0);
        a.ret();
        a.bind(big);
        a.push(Rcx);
        a.mov_rr(Rcx, Rax); // rcx = v
        a.mov_rr(R10, Rax);
        a.neg(R10);
        a.cmov(Cc::S, R10, Rax); // r10 = |v|
        a.mov_ri(R11, 1 << 30);
        a.cmp_rr(R10, R11);
        a.jcc(Cc::AE, slow);
        a.lea(R11, M(R15, tls));
        a.load(Rax, M(R11, heads + 8 * ci));
        a.test_rr(Rax, Rax);
        a.jcc(Cc::E, slow);
        a.push(Rdx);
        a.load(Rdx, M(Rax, 0));
        a.store(M(R11, heads + 8 * ci), Rdx);
        a.add32_mi(M(R11, counts + 4 * ci), -1);
        a.mov_rr(Rdx, R10); // |v|
        hdr(a, int_t as i64);
        a.mov_ri(R10, 8);
        a.mov_ri(R11, 10);
        a.test_rr(Rcx, Rcx);
        a.cmov(Cc::S, R10, R11);
        a.store(M(Rax, LV_TAG), R10);
        a.store32(M(Rax, LV_DIGIT), Rdx);
        a.pop(Rdx);
        a.pop(Rcx);
        a.ret();
        a.bind(slow);
        a.mov_rr(Rax, Rcx);
        a.pop(Rcx);
        a.bind(slow2);
        // rsp % 16 == 8 here; push + save_all (192 bytes) align the call
        a.push(Rdi);
        a.mov_rr(Rdi, Rax);
        save_all(a);
        a.call_abs(fa(h::h_new_int as unsafe extern "C" fn(_) -> _) as usize);
        restore_all(a);
        a.pop(Rdi);
        a.ret();
    }
    // ---- box_float
    a.bind(box_float);
    {
        let slow = a.label();
        a.lea(R11, M(R15, tls));
        a.load(Rax, M(R11, heads + 8 * cf));
        a.test_rr(Rax, Rax);
        a.jcc(Cc::E, slow);
        a.load(R10, M(Rax, 0));
        a.store(M(R11, heads + 8 * cf), R10);
        a.add32_mi(M(R11, counts + 4 * cf), -1);
        hdr(a, float_t as i64);
        a.movsd_store(M(Rax, FVAL), XS0);
        a.ret();
        a.bind(slow);
        // rsp % 16 == 8; xmm0 holds the argument
        preserving_call_x(a, fa(h::h_new_float as unsafe extern "C" fn(_) -> _));
        a.ret();
    }
    // ---- decref_shared (rax = object)
    a.bind(dshared);
    {
        a.push(Rdi);
        a.mov_rr(Rdi, Rax);
        save_all(a);
        a.call_abs(fa(h::h_decref_shared as unsafe extern "C" fn(_)) as usize);
        restore_all(a);
        a.pop(Rdi);
        a.ret();
    }
    // ---- merge_zero (rax = object whose local refcount reached 0)
    a.bind(mzero);
    {
        let generic = a.label();
        let ok = a.label();
        a.load(R10, M(Rax, OB_TYPE));
        a.mov_ri(R11, int_t as i64);
        a.cmp_rr(R10, R11);
        a.jcc(Cc::E, ok);
        a.mov_ri(R11, float_t as i64);
        a.cmp_rr(R10, R11);
        a.jcc(Cc::NE, generic);
        a.bind(ok);
        a.cmp_mi(M(Rax, OB_REF_SHARED), 0);
        a.jcc(Cc::NE, generic);
        a.load8(R10, M(Rax, GC_BITS));
        a.test_rr(R10, R10);
        a.jcc(Cc::NE, generic);
        a.mov_rr(R10, Rax);
        a.and_ri(R10, !(crate::tlab::PAGE as i32 - 1));
        a.load32(R10, M(R10, tl.page_class as i32));
        a.lea(R11, M(R15, tls));
        a.cmp32_idx_i(R11, R10, 4, counts, tl.limit as i32);
        a.jcc(Cc::AE, generic);
        a.push(Rcx);
        a.load_idx(Rcx, R11, R10, 8, heads);
        a.store(M(Rax, 0), Rcx);
        a.store_idx(R11, R10, 8, heads, Rax);
        a.add32_idx_i(R11, R10, 4, counts, 1);
        a.pop(Rcx);
        a.ret();
        a.bind(generic);
        a.push(Rdi);
        a.mov_rr(Rdi, Rax);
        save_all(a);
        a.call_abs(fa(h::h_merge_zero as unsafe extern "C" fn(_)) as usize);
        restore_all(a);
        a.pop(Rdi);
        a.ret();
    }
    let _ = preserving_call;
}

/// Preserving call with the argument in xmm0 (entered with rsp % 16 == 8).
fn preserving_call_x(a: &mut Asm, f: i64) {
    // save_all stores xmm0 too; reload it after saving
    a.sub_ri(Rsp, 8);
    save_all(a);
    a.movsd_load(XS0, M(Rsp, 0));
    a.call_abs(f as usize);
    restore_all(a);
    a.add_ri(Rsp, 8);
}

pub fn codegen(u: &Unit, built: &Built, al: &Alloc, deopt_counter: usize) -> Result<Out, String> {
    let f = &built.func;
    let mut asm = Asm::new();
    let blabel: Vec<Label> = (0..f.blocks.len()).map(|_| asm.label()).collect();
    // tier-1 instruction labels: block starts map to tier-2 block labels
    let ilabel: Vec<Label> = (0..u.an.instrs.len())
        .map(|i| if built.instr_block[i] != u32::MAX { blabel[built.instr_block[i] as usize] } else { asm.label() })
        .collect();
    let exit = asm.label();
    let (rt_box_int, rt_box_float, rt_decref_shared, rt_merge_zero) = (asm.label(), asm.label(), asm.label(), asm.label());
    let flags = u.flags;
    let g = Gen::new_embedded(asm, u.an, u.cd, u.nlp, flags, u.tables, ilabel, exit);
    let mut cg = Cg {
        g,
        f,
        al,
        nlp: u.nlp as i32,
        blabel,
        exit,
        snap_label: vec![None; f.snaps.len()],
        rt_box_int,
        rt_box_float,
        rt_decref_shared,
        rt_merge_zero,
        deopt_counter,
        sites: Vec::new(),
        forced: Vec::new(),
        ret_syncs: Vec::new(),
        poll_back: Vec::new(),
        sp_bias: 0,
    };
    let frame = al.frame;
    // ---- trampoline (tier-1 contract): fn(frame, slots, ts, target, out)
    {
        let a = cg.a();
        a.push(Rbp);
        a.mov_rr(Rbp, Rsp);
        a.push(Rbx);
        a.push(R12);
        a.push(R13);
        a.push(R14);
        a.push(R15);
        a.sub_ri(Rsp, 8);
        a.store(M(Rsp, 0), R8);
        a.mov_rr(Rbx, Rdi);
        a.mov_rr(R12, Rsi);
        a.mov_rr(R13, Rdx);
        a.load_fs0(R15);
        a.jmp_r(Rcx);
    }
    // ---- exit: drop the tier-2 frame, then the tier-1 epilogue
    let tier1_exit = cg.a().label();
    {
        let a = cg.a();
        a.bind(exit);
        a.store_i(M(Rbx, T2_SITE), 0);
        if frame > 0 {
            a.add_ri(Rsp, frame);
        }
        a.bind(tier1_exit);
        a.add_ri(Rsp, 8);
        a.pop(R15);
        a.pop(R14);
        a.pop(R13);
        a.pop(R12);
        a.pop(Rbx);
        a.pop(Rbp);
        a.ret();
    }
    // ---- entries
    let mut entries = Vec::new();
    // (code unit pc, label of its entry stub): generator suspensions record the
    // address of the entry they resume at
    let mut entry_labels: Vec<(u32, crate::interp::jit::x64::Label)> = Vec::new();
    for &(pc, b) in &f.entries {
        let pos = cg.a().pos() as u32;
        entries.push((pc, pos));
        let el = cg.a().label();
        cg.a().bind(el);
        entry_labels.push((pc, el));
        // invalidated code is never entered: resume in the interpreter instead
        {
            let depth = u.an.depth[u.an.index_of[pc as usize] as usize].max(0);
            let nlp = cg.nlp;
            let ok = cg.a().label();
            let a = cg.a();
            a.mov_ri(R10, u.invalid_flag as i64);
            a.cmp8_mi(M(R10, 0), 0);
            a.jcc(Cc::E, ok);
            a.store32_i(M(Rbx, NEXT_INSTR), pc as i32);
            a.store_i(M(Rbx, STACK_TOP), nlp + depth);
            a.mov_ri(Rax, tier1::ST_DEOPT as i64);
            a.jmp(tier1_exit);
            a.bind(ok);
        }
        if frame > 0 {
            cg.a().sub_ri(Rsp, frame);
        }
        cg.a().store(M(Rbx, T2_SP), Rsp);
        let l = cg.blabel[b as usize];
        cg.a().jmp(l);
    }
    // ---- blocks
    for (k, &b) in f.order.iter().enumerate() {
        let blk = &f.blocks[b as usize];
        let l = cg.blabel[b as usize];
        cg.a().bind(l);
        for ins in &blk.insts {
            cg.inst(ins)?;
        }
        let next = f.order.get(k + 1).copied();
        match &blk.term {
            Term::Jump(t) => {
                if Some(*t) != next {
                    let l = cg.blabel[*t as usize];
                    cg.a().jmp(l);
                }
            }
            Term::Branch(c, t, e) => {
                let lt = cg.blabel[*t as usize];
                let le = cg.blabel[*e as usize];
                if Some(*t) == next {
                    // jump to e when !c
                    let skip = cg.a().label();
                    cg.jump_if(c, skip);
                    cg.a().jmp(le);
                    cg.a().bind(skip);
                } else {
                    cg.jump_if(c, lt);
                    if Some(*e) != next {
                        cg.a().jmp(le);
                    }
                }
            }
            Term::Return(v, depth, snap) => {
                if let Some(s) = snap {
                    // a frame object outlives the activation: it must see the final
                    // values of unboxed locals
                    let stub = cg.a().label();
                    let back = cg.a().label();
                    cg.a().cmp_mi(M(Rbx, FRAME_OBJ), 0);
                    cg.a().jcc(Cc::NE, stub);
                    cg.a().bind(back);
                    cg.ret_syncs.push((stub, back, *s));
                }
                let r = cg.gr(*v, Rax);
                let nlp = cg.nlp;
                let a = cg.a();
                if r != Rax {
                    a.mov_rr(Rax, r);
                }
                a.load(R10, M(Rsp, frame));
                a.store(M(R10, 0), Rax);
                a.store_i(M(Rbx, STACK_TOP), nlp + *depth as i32);
                a.mov_ri(Rax, tier1::ST_RETURN as i64);
                let e = cg.exit;
                cg.a().jmp(e);
            }
            Term::Yield(v, depth, s, yfs) => {
                // result first (the local syncs clobber registers), then the frame
                let r = cg.gr(*v, Rax);
                {
                    let a = cg.a();
                    if r != Rax {
                        a.mov_rr(Rax, r);
                    }
                    a.load(R10, M(Rsp, frame));
                    a.store(M(R10, 0), Rax);
                }
                let snap = f.snaps[*s as usize].clone();
                cg.sync_snapshot_locals(&snap);
                let cont = entry_labels.iter().find(|e| e.0 == snap.resume).map(|e| e.1).ok_or("tier2: yield without resume entry")?;
                let nlp = cg.nlp;
                let a = cg.a();
                a.store32_i(M(Rbx, LASTI), snap.lasti as i32);
                a.store32_i(M(Rbx, NEXT_INSTR), snap.resume as i32);
                a.store32_i(M(Rbx, YIELD_FROM_SEND), *yfs);
                a.store_i(M(Rbx, STACK_TOP), nlp + *depth as i32);
                a.mov_r_label_addr(Rcx, cont);
                a.store(M(Rbx, RESUME), Rcx);
                a.mov_ri(Rax, tier1::ST_YIELD as i64);
                let e = cg.exit;
                cg.a().jmp(e);
            }
            Term::Deopt(s) => {
                let l = cg.snap_target(*s);
                cg.a().jmp(l);
            }
            Term::None => {
                cg.a().ud2();
            }
        }
    }
    // ---- snapshot stubs (stubs may reference other snapshots: until none is left)
    let mut emitted = vec![false; f.snaps.len()];
    loop {
        let mut progress = false;
        for s in 0..f.snaps.len() {
            if !emitted[s] && cg.snap_label[s].is_some() {
                emitted[s] = true;
                progress = true;
                cg.snapshot_stub(s);
            }
        }
        if !progress {
            break;
        }
    }
    cg.emit_forced_and_ret_syncs();
    emit_routines(cg.a(), rt_box_int, rt_box_float, rt_decref_shared, rt_merge_zero);
    let _ = tier1_exit;
    let sites = std::mem::take(&mut cg.sites);
    let asm = cg.g.finish()?;
    Ok(Out { asm, entries, sites })
}
