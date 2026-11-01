//! Linear-scan register allocation (Poletto & Sarkar style) over the linearised
//! tier-2 IR. Each vreg gets one live interval [first, last] (holes are not
//! modelled); intervals crossing an instruction that clobbers caller-saved
//! registers may only use registers preserved there (rbp, and r14 unless a template
//! instruction is crossed); otherwise they are spilled to a native stack slot.

use super::ir::*;
use crate::interp::jit::x64::R;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loc {
    Reg(R),
    Xmm(u8),
    /// native stack slot: [rsp + off]
    Stack(i32),
    /// never defined / dead
    None,
}

pub struct Alloc {
    pub loc: Vec<Loc>,
    /// native frame size in bytes (multiple of 16)
    pub frame: i32,
}

/// Allocatable caller-saved gprs (rax, r10, r11 are codegen scratch).
const GPR_CALLER: [R; 6] = [R::Rcx, R::Rdx, R::Rsi, R::Rdi, R::R8, R::R9];
/// Allocatable xmm registers (xmm0, xmm1, xmm15 are codegen scratch).
const XMM_ALLOC: std::ops::Range<u8> = 2..15;

pub fn allocate(f: &Func) -> Alloc {
    let nv = f.classes.len();
    let nb = f.blocks.len();
    // ---- positions
    let mut pos_of_block = vec![(0u32, 0u32); nb];
    let mut pos = 0u32;
    for &b in &f.order {
        let start = pos;
        pos += 2 * (f.blocks[b as usize].insts.len() as u32 + 1);
        pos_of_block[b as usize] = (start, pos - 1);
    }
    // ---- per-block use/def (uses before defs)
    let mut uses_b: Vec<Vec<V>> = vec![Vec::new(); nb];
    let mut defs_b: Vec<Vec<bool>> = vec![Vec::new(); nb];
    let mut u = Vec::new();
    let mut d = Vec::new();
    let snap_uses = |s: Option<SnapId>, out: &mut Vec<V>| {
        if let Some(s) = s {
            f.snaps[s as usize].vregs(out);
        }
    };
    for b in 0..nb {
        let mut defined = vec![false; nv];
        let mut ub = Vec::new();
        for ins in &f.blocks[b].insts {
            u.clear();
            d.clear();
            ins.uses_defs(&mut u, &mut d);
            snap_uses(ins.snap(), &mut u);
            for &x in &u {
                if !defined[x as usize] {
                    ub.push(x);
                }
            }
            for &x in &d {
                defined[x as usize] = true;
            }
        }
        u.clear();
        f.blocks[b].term.uses(&mut u);
        if let Term::Deopt(s) = f.blocks[b].term {
            f.snaps[s as usize].vregs(&mut u);
        }
        for &x in &u {
            if !defined[x as usize] {
                ub.push(x);
            }
        }
        uses_b[b] = ub;
        defs_b[b] = defined;
    }
    // ---- liveness (bit sets as Vec<u64>)
    let words = nv.div_ceil(64).max(1);
    let mut live_in = vec![vec![0u64; words]; nb];
    let mut live_out = vec![vec![0u64; words]; nb];
    let succs: Vec<Vec<BlockId>> = f.blocks.iter().map(|b| b.term.succs()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &b in f.order.iter().rev() {
            let b = b as usize;
            let mut out = vec![0u64; words];
            for &s in &succs[b] {
                for w in 0..words {
                    out[w] |= live_in[s as usize][w];
                }
            }
            let mut inn = out.clone();
            for x in 0..nv {
                if defs_b[b][x] {
                    inn[x / 64] &= !(1 << (x % 64));
                }
            }
            for &x in &uses_b[b] {
                inn[x as usize / 64] |= 1 << (x % 64);
            }
            if inn != live_in[b] || out != live_out[b] {
                live_in[b] = inn;
                live_out[b] = out;
                changed = true;
            }
        }
    }
    // ---- intervals
    let mut lo = vec![u32::MAX; nv];
    let mut hi = vec![0u32; nv];
    let mut ext = |x: usize, p: u32, lo: &mut Vec<u32>, hi: &mut Vec<u32>| {
        if p < lo[x] {
            lo[x] = p;
        }
        if p > hi[x] {
            hi[x] = p;
        }
    };
    let mut clobbers: Vec<(u32, bool)> = Vec::new();
    for &b in &f.order {
        let bi = b as usize;
        let (start, end) = pos_of_block[bi];
        for x in 0..nv {
            if live_in[bi][x / 64] >> (x % 64) & 1 != 0 {
                ext(x, start, &mut lo, &mut hi);
            }
            if live_out[bi][x / 64] >> (x % 64) & 1 != 0 {
                ext(x, end, &mut lo, &mut hi);
            }
        }
        let mut p = start;
        for ins in &f.blocks[bi].insts {
            u.clear();
            d.clear();
            ins.uses_defs(&mut u, &mut d);
            snap_uses(ins.snap(), &mut u);
            for &x in &u {
                ext(x as usize, p, &mut lo, &mut hi);
            }
            for &x in &d {
                ext(x as usize, p + 1, &mut lo, &mut hi);
            }
            if ins.clobbers_caller_saved() {
                clobbers.push((p, ins.clobbers_r14()));
            }
            p += 2;
        }
        u.clear();
        f.blocks[bi].term.uses(&mut u);
        if let Term::Deopt(s) = f.blocks[bi].term {
            f.snaps[s as usize].vregs(&mut u);
        }
        for &x in &u {
            ext(x as usize, p, &mut lo, &mut hi);
        }
    }
    // ---- linear scan
    let crosses = |a: u32, b: u32| -> (bool, bool) {
        // (crosses any clobber, crosses a template)
        let mut any = false;
        let mut tpl = false;
        for &(p, t) in &clobbers {
            if p > a && p < b {
                any = true;
                tpl |= t;
            }
        }
        (any, tpl)
    };
    let mut order: Vec<usize> = (0..nv).filter(|&x| lo[x] != u32::MAX).collect();
    order.sort_by_key(|&x| lo[x]);
    let mut loc = vec![Loc::None; nv];
    let mut nslots = 0i32;
    // active: (end, vreg)
    let mut active: Vec<(u32, usize)> = Vec::new();
    let mut spill = |nslots: &mut i32| -> Loc {
        let l = Loc::Stack(8 * *nslots);
        *nslots += 1;
        l
    };
    for &x in &order {
        let (s, e) = (lo[x], hi[x]);
        active.retain(|&(end, _)| end >= s);
        let (cross, cross_tpl) = crosses(s, e);
        let used: Vec<Loc> = active.iter().map(|&(_, v)| loc[v]).collect();
        let pick = match f.classes[x] {
            Class::G => {
                let mut cands: Vec<R> = Vec::new();
                if !cross {
                    cands.extend_from_slice(&GPR_CALLER);
                }
                if !cross_tpl {
                    cands.push(R::R14);
                }
                cands.push(R::Rbp);
                cands.into_iter().map(Loc::Reg).find(|l| !used.contains(l))
            }
            Class::F => {
                if cross {
                    None
                } else {
                    XMM_ALLOC.map(Loc::Xmm).find(|l| !used.contains(l))
                }
            }
        };
        match pick {
            Some(l) => {
                loc[x] = l;
                active.push((e, x));
            }
            None => {
                // spill the active interval of the same class ending last if it ends
                // after this one and its register would be legal for us
                let mut best: Option<usize> = None;
                for (k, &(end, v)) in active.iter().enumerate() {
                    if f.classes[v] != f.classes[x] || end <= e {
                        continue;
                    }
                    let ok = match loc[v] {
                        Loc::Reg(r) => {
                            if cross {
                                r == R::Rbp || (r == R::R14 && !cross_tpl)
                            } else {
                                r != R::R14 || !cross_tpl
                            }
                        }
                        Loc::Xmm(_) => !cross,
                        _ => false,
                    };
                    if ok && best.is_none_or(|b| active[b].0 < end) {
                        best = Some(k);
                    }
                }
                match best {
                    Some(k) => {
                        let v = active[k].1;
                        loc[x] = loc[v];
                        loc[v] = spill(&mut nslots);
                        active[k] = (e, x);
                    }
                    None => loc[x] = spill(&mut nslots),
                }
            }
        }
    }
    let frame = ((nslots * 8 + 15) / 16) * 16;
    Alloc { loc, frame }
}
