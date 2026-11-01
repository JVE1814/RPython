//! Match / search drivers with search acceleration.

use crate::vm::{CodeUnit, Scratch, Vm, UNSET};
use crate::{Captures, MatchMode, Pattern};

/// Find the first index `i` in `s` with `s[i] == c`.
#[inline]
fn find_unit<T: CodeUnit>(s: &[T], c: u32) -> Option<usize> {
    if std::mem::size_of::<T>() == 1 {
        if c > 255 {
            return None;
        }
        // SAFETY: T is u8 (the only 1-byte CodeUnit).
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, s.len()) };
        return memchr(c as u8, bytes);
    }
    if std::mem::size_of::<T>() == 2 && c > 0xFFFF {
        return None;
    }
    s.iter().position(|&x| x.into() == c)
}

/// Portable word-at-a-time memchr.
pub(crate) fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let n = hay.len();
    let mut i = 0;
    let rep = LO.wrapping_mul(needle as u64);
    while i + 8 <= n {
        let w = u64::from_le_bytes(hay[i..i + 8].try_into().unwrap()) ^ rep;
        if (w.wrapping_sub(LO) & !w & HI) != 0 {
            break;
        }
        i += 8;
    }
    hay[i..].iter().position(|&b| b == needle).map(|p| p + i)
}

fn build<T: CodeUnit>(vm: &Vm<'_, T>, start: usize, end: usize, groups: usize) -> Captures {
    let mut spans = Vec::with_capacity(groups + 1);
    spans.push((start as isize, end as isize));
    let slots = vm.slots();
    for g in 0..groups {
        let a = slots[2 * g];
        let b = slots[2 * g + 1];
        if a == UNSET || b == UNSET || a > b {
            spans.push((-1, -1));
        } else {
            spans.push((a as isize, b as isize));
        }
    }
    let li = vm.lastindex();
    Captures { spans, lastindex: if li > 0 { Some(li as usize) } else { None } }
}

pub(crate) fn exec<T: CodeUnit>(
    pat: &Pattern,
    sc: &mut Scratch,
    s: &[T],
    pos: usize,
    end: usize,
    mode: MatchMode,
    must_advance: bool,
) -> Option<Captures> {
    let prog = &pat.prog;
    let groups = pat.groups;
    let mut vm = Vm::new(prog, &pat.prov, s, end, sc);
    vm.match_all = mode == MatchMode::Full;
    vm.must_advance = must_advance;
    vm.origin = pos;
    if mode != MatchMode::Search {
        if prog.anchored_start && pos != 0 {
            return None;
        }
        if end - pos < prog.min_width {
            return None;
        }
        let e = vm.run(0, pos)?;
        return Some(build(&vm, pos, e, groups));
    }
    // ---- search
    if prog.anchored_start {
        if pos != 0 || end < prog.min_width {
            return None;
        }
        let e = vm.run(0, 0)?;
        return Some(build(&vm, 0, e, groups));
    }
    if end - pos < prog.min_width {
        return None;
    }
    let last = end - prog.min_width;
    let plen = prog.prefix.len();
    if plen > 0 {
        let c0 = prog.prefix[0];
        let rest = &prog.prefix[1..];
        let mut i = pos;
        while i <= last {
            let off = find_unit(&s[i..last + 1], c0)?;
            i += off;
            // verify rest of prefix
            if s[i + 1..i + plen].iter().zip(rest).all(|(&a, &b)| a.into() == b) {
                if prog.pure_literal {
                    return Some(Captures {
                        spans: vec![(i as isize, (i + plen) as isize)],
                        lastindex: None,
                    });
                }
                if let Some(e) = vm.run(prog.prefix_pc as usize, i + plen) {
                    return Some(build(&vm, i, e, groups));
                }
            }
            i += 1;
        }
        return None;
    }
    if !prog.pairs.is_empty() {
        let pairs = &prog.pairs[..];
        let mut i = pos;
        while i <= last {
            let ok = pairs.iter().any(|g| {
                i + g.len() <= end && g.iter().enumerate().all(|(j, set)| set.test(s[i + j].into()))
            });
            if ok {
                if let Some(e) = vm.run(0, i) {
                    return Some(build(&vm, i, e, groups));
                }
            }
            i += 1;
        }
        return None;
    }
    if let Some(first) = &prog.first {
        let mut i = pos;
        while i <= last && i < end {
            // skip positions whose first char cannot start a match
            let c: u32 = s[i].into();
            if first.test(c) {
                if let Some(e) = vm.run(0, i) {
                    return Some(build(&vm, i, e, groups));
                }
            }
            i += 1;
        }
        return None;
    }
    let mut i = pos;
    loop {
        if let Some(e) = vm.run(0, i) {
            return Some(build(&vm, i, e, groups));
        }
        if i >= last {
            return None;
        }
        i += 1;
    }
}
