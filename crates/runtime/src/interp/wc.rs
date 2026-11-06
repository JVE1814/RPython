//! Public CPython 3.15 wordcode: opcode numbers (spec/Include/opcode_ids.h), inline-cache
//! sizes, decoding of instructions, of the exception table and of the location table.
//!
//! bc-vm keeps this small runtime-side view; the compiler side (`rpy_compiler::wordcode`,
//! bc-compiler) owns the full metadata used by `dis`. Both must agree on `cache_entries`.

#![allow(dead_code)]

pub mod op {
    pub const CACHE: u8 = 0;
    pub const BINARY_SLICE: u8 = 1;
    pub const BUILD_TEMPLATE: u8 = 2;
    pub const BINARY_OP_INPLACE_ADD_UNICODE: u8 = 3;
    pub const CALL_FUNCTION_EX: u8 = 4;
    pub const CHECK_EG_MATCH: u8 = 5;
    pub const CHECK_EXC_MATCH: u8 = 6;
    pub const CLEANUP_THROW: u8 = 7;
    pub const DELETE_SUBSCR: u8 = 8;
    pub const END_FOR: u8 = 9;
    pub const END_SEND: u8 = 10;
    pub const EXIT_INIT_CHECK: u8 = 11;
    pub const FORMAT_SIMPLE: u8 = 12;
    pub const FORMAT_WITH_SPEC: u8 = 13;
    pub const GET_AITER: u8 = 14;
    pub const GET_ANEXT: u8 = 15;
    pub const GET_LEN: u8 = 16;
    pub const RESERVED: u8 = 17;
    pub const INTERPRETER_EXIT: u8 = 18;
    pub const LOAD_BUILD_CLASS: u8 = 19;
    pub const LOAD_LOCALS: u8 = 20;
    pub const MAKE_FUNCTION: u8 = 21;
    pub const MATCH_KEYS: u8 = 22;
    pub const MATCH_MAPPING: u8 = 23;
    pub const MATCH_SEQUENCE: u8 = 24;
    pub const NOP: u8 = 25;
    pub const NOT_TAKEN: u8 = 26;
    pub const POP_EXCEPT: u8 = 27;
    pub const POP_ITER: u8 = 28;
    pub const POP_TOP: u8 = 29;
    pub const PUSH_EXC_INFO: u8 = 30;
    pub const PUSH_NULL: u8 = 31;
    pub const RETURN_GENERATOR: u8 = 32;
    pub const RETURN_VALUE: u8 = 33;
    pub const SETUP_ANNOTATIONS: u8 = 34;
    pub const STORE_SLICE: u8 = 35;
    pub const STORE_SUBSCR: u8 = 36;
    pub const TO_BOOL: u8 = 37;
    pub const UNARY_INVERT: u8 = 38;
    pub const UNARY_NEGATIVE: u8 = 39;
    pub const UNARY_NOT: u8 = 40;
    pub const WITH_EXCEPT_START: u8 = 41;
    pub const BINARY_OP: u8 = 42;
    pub const BUILD_INTERPOLATION: u8 = 43;
    pub const BUILD_LIST: u8 = 44;
    pub const BUILD_MAP: u8 = 45;
    pub const BUILD_SET: u8 = 46;
    pub const BUILD_SLICE: u8 = 47;
    pub const BUILD_STRING: u8 = 48;
    pub const BUILD_TUPLE: u8 = 49;
    pub const CALL: u8 = 50;
    pub const CALL_INTRINSIC_1: u8 = 51;
    pub const CALL_INTRINSIC_2: u8 = 52;
    pub const CALL_KW: u8 = 53;
    pub const COMPARE_OP: u8 = 54;
    pub const CONTAINS_OP: u8 = 55;
    pub const CONVERT_VALUE: u8 = 56;
    pub const COPY: u8 = 57;
    pub const COPY_FREE_VARS: u8 = 58;
    pub const DELETE_ATTR: u8 = 59;
    pub const DELETE_DEREF: u8 = 60;
    pub const DELETE_FAST: u8 = 61;
    pub const DELETE_GLOBAL: u8 = 62;
    pub const DELETE_NAME: u8 = 63;
    pub const DICT_MERGE: u8 = 64;
    pub const DICT_UPDATE: u8 = 65;
    pub const END_ASYNC_FOR: u8 = 66;
    pub const EXTENDED_ARG: u8 = 67;
    pub const FOR_ITER: u8 = 68;
    pub const GET_AWAITABLE: u8 = 69;
    pub const GET_ITER: u8 = 70;
    pub const IMPORT_FROM: u8 = 71;
    pub const IMPORT_NAME: u8 = 72;
    pub const IS_OP: u8 = 73;
    pub const JUMP_BACKWARD: u8 = 74;
    pub const JUMP_BACKWARD_NO_INTERRUPT: u8 = 75;
    pub const JUMP_FORWARD: u8 = 76;
    pub const LIST_APPEND: u8 = 77;
    pub const LIST_EXTEND: u8 = 78;
    pub const LOAD_ATTR: u8 = 79;
    pub const LOAD_COMMON_CONSTANT: u8 = 80;
    pub const LOAD_CONST: u8 = 81;
    pub const LOAD_DEREF: u8 = 82;
    pub const LOAD_FAST: u8 = 83;
    pub const LOAD_FAST_AND_CLEAR: u8 = 84;
    pub const LOAD_FAST_BORROW: u8 = 85;
    pub const LOAD_FAST_BORROW_LOAD_FAST_BORROW: u8 = 86;
    pub const LOAD_FAST_CHECK: u8 = 87;
    pub const LOAD_FAST_LOAD_FAST: u8 = 88;
    pub const LOAD_FROM_DICT_OR_DEREF: u8 = 89;
    pub const LOAD_FROM_DICT_OR_GLOBALS: u8 = 90;
    pub const LOAD_GLOBAL: u8 = 91;
    pub const LOAD_NAME: u8 = 92;
    pub const LOAD_SMALL_INT: u8 = 93;
    pub const LOAD_SPECIAL: u8 = 94;
    pub const LOAD_SUPER_ATTR: u8 = 95;
    pub const MAKE_CELL: u8 = 96;
    pub const MAP_ADD: u8 = 97;
    pub const MATCH_CLASS: u8 = 98;
    pub const POP_JUMP_IF_FALSE: u8 = 99;
    pub const POP_JUMP_IF_NONE: u8 = 100;
    pub const POP_JUMP_IF_NOT_NONE: u8 = 101;
    pub const POP_JUMP_IF_TRUE: u8 = 102;
    pub const RAISE_VARARGS: u8 = 103;
    pub const RERAISE: u8 = 104;
    pub const SEND: u8 = 105;
    pub const SET_ADD: u8 = 106;
    pub const SET_FUNCTION_ATTRIBUTE: u8 = 107;
    pub const SET_UPDATE: u8 = 108;
    pub const STORE_ATTR: u8 = 109;
    pub const STORE_DEREF: u8 = 110;
    pub const STORE_FAST: u8 = 111;
    pub const STORE_FAST_LOAD_FAST: u8 = 112;
    pub const STORE_FAST_STORE_FAST: u8 = 113;
    pub const STORE_GLOBAL: u8 = 114;
    pub const STORE_NAME: u8 = 115;
    pub const SWAP: u8 = 116;
    pub const UNPACK_EX: u8 = 117;
    pub const UNPACK_SEQUENCE: u8 = 118;
    pub const YIELD_VALUE: u8 = 119;
    pub const RESUME: u8 = 128;
    pub const MIN_SPECIALIZED: u8 = 129;
    pub const MIN_INSTRUMENTED: u8 = 233;
    pub const INSTRUMENTED_LINE: u8 = 253;
    pub const INSTRUMENTED_INSTRUCTION: u8 = 237;
    pub const ENTER_EXECUTOR: u8 = 254;
    pub const HAVE_ARGUMENT: u8 = 41;
}

/// Base (unspecialised, uninstrumented) opcode, or None for numbers that never denote a
/// real instruction of their own (CACHE, unused numbers, INSTRUMENTED_LINE, ...).
pub fn deopt(o: u8) -> Option<u8> {
    use op::*;
    Some(match o {
        CACHE => return None,
        1..=119 if o != BINARY_OP_INPLACE_ADD_UNICODE => o,
        BINARY_OP_INPLACE_ADD_UNICODE => BINARY_OP,
        RESUME => RESUME,
        129..=143 => BINARY_OP,
        144..=146 => CALL,
        147..=150 => CALL,
        151 | 152 => CALL_FUNCTION_EX,
        153 => CALL,
        154..=156 => CALL_KW,
        157..=168 => CALL,
        169..=171 => COMPARE_OP,
        172 | 173 => CONTAINS_OP,
        174..=178 => FOR_ITER,
        179 | 180 => GET_ITER,
        181 | 182 => JUMP_BACKWARD,
        183..=195 => LOAD_ATTR,
        196 | 197 => LOAD_GLOBAL,
        198 | 199 => LOAD_SUPER_ATTR,
        200 | 201 => RESUME,
        202..=204 => SEND,
        205..=207 => STORE_ATTR,
        208 | 209 => STORE_SUBSCR,
        210..=215 => TO_BOOL,
        216..=218 => UNPACK_SEQUENCE,
        233 => END_FOR,
        234 => POP_ITER,
        235 => END_SEND,
        236 => FOR_ITER,
        238 => JUMP_FORWARD,
        239 => NOT_TAKEN,
        240 => POP_JUMP_IF_TRUE,
        241 => POP_JUMP_IF_FALSE,
        242 => POP_JUMP_IF_NONE,
        243 => POP_JUMP_IF_NOT_NONE,
        244 => RESUME,
        245 => RETURN_VALUE,
        246 => YIELD_VALUE,
        247 => END_ASYNC_FOR,
        248 => LOAD_SUPER_ATTR,
        249 => CALL,
        250 => CALL_KW,
        251 => CALL_FUNCTION_EX,
        252 => JUMP_BACKWARD,
        _ => return None,
    })
}

/// Number of inline CACHE code units following a (base) instruction.
pub fn cache_entries(base: u8) -> u32 {
    rpy_compiler::wordcode::cache_entries(base as u16)
}

#[allow(dead_code)]
fn cache_entries_local(base: u8) -> u32 {
    use op::*;
    match base {
        LOAD_GLOBAL => 4,
        BINARY_OP => 5,
        UNPACK_SEQUENCE | COMPARE_OP | CONTAINS_OP | FOR_ITER | LOAD_SUPER_ATTR | STORE_SUBSCR | SEND
        | JUMP_BACKWARD | POP_JUMP_IF_FALSE | POP_JUMP_IF_TRUE | POP_JUMP_IF_NONE | POP_JUMP_IF_NOT_NONE | RESUME
        | GET_ITER | CALL_FUNCTION_EX => 1,
        LOAD_ATTR => 9,
        STORE_ATTR => 4,
        CALL | CALL_KW | TO_BOOL => 3,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Instr {
    /// code unit of the opcode itself (after EXTENDED_ARG prefixes)
    pub offset: u32,
    /// code unit of the first EXTENDED_ARG prefix (== offset without prefixes)
    pub start: u32,
    /// opcode as found in co_code (may be specialised/instrumented)
    pub raw: u8,
    /// base opcode (None: not a valid instruction)
    pub op: Option<u8>,
    pub arg: u32,
    pub ncache: u32,
}

impl Instr {
    /// code unit following this instruction and its caches
    pub fn next(&self) -> u32 {
        self.offset + 1 + self.ncache
    }
}

/// Decode co_code into instructions (CACHE units skipped, EXTENDED_ARG folded).
pub fn instructions(code: &[u8]) -> Vec<Instr> {
    let n = code.len() / 2;
    let mut v = Vec::with_capacity(n);
    let mut i = 0usize;
    let mut ext: u32 = 0;
    let mut start = 0usize;
    while i < n {
        let raw = code[2 * i];
        let a = code[2 * i + 1] as u32;
        let base = deopt(raw);
        if base == Some(op::EXTENDED_ARG) {
            ext = (ext | a) << 8;
            i += 1;
            continue;
        }
        let arg = ext | a;
        let ncache = base.map_or(0, cache_entries);
        v.push(Instr { offset: i as u32, start: start as u32, raw, op: base, arg, ncache });
        ext = 0;
        i += 1 + ncache as usize;
        start = i;
    }
    v
}

/// Absolute jump target (code units) of a jump instruction, if `op` is one.
pub fn jump_target(ins: &Instr) -> Option<u32> {
    use op::*;
    let next = ins.next() as i64;
    let t = match ins.op? {
        JUMP_FORWARD | POP_JUMP_IF_FALSE | POP_JUMP_IF_TRUE | POP_JUMP_IF_NONE | POP_JUMP_IF_NOT_NONE | FOR_ITER | SEND => {
            next + ins.arg as i64
        }
        JUMP_BACKWARD | JUMP_BACKWARD_NO_INTERRUPT => next - ins.arg as i64,
        _ => return None,
    };
    if t < 0 { Some(u32::MAX) } else { Some(t as u32) }
}

// ------------------------------------------------------------------ exception table

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExcEntry {
    pub start: u32,
    /// exclusive
    pub end: u32,
    pub target: u32,
    pub depth: u32,
    pub lasti: bool,
}

fn read_be_varint(t: &[u8], pos: &mut usize) -> Option<u32> {
    let mut b = *t.get(*pos)?;
    *pos += 1;
    let mut v = (b & 63) as u32;
    while b & 64 != 0 {
        b = *t.get(*pos)?;
        *pos += 1;
        v = (v << 6) | (b & 63) as u32;
    }
    Some(v)
}

/// Decode co_exceptiontable (None if malformed).
pub fn decode_exception_table(t: &[u8]) -> Option<Vec<ExcEntry>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < t.len() {
        if t[pos] & 128 == 0 {
            return None;
        }
        let start = read_be_varint(t, &mut pos)?;
        let len = read_be_varint(t, &mut pos)?;
        let target = read_be_varint(t, &mut pos)?;
        let dl = read_be_varint(t, &mut pos)?;
        out.push(ExcEntry { start, end: start + len, target, depth: dl >> 1, lasti: dl & 1 != 0 });
    }
    Some(out)
}

fn write_be_varint(out: &mut Vec<u8>, v: u32, first: bool) {
    let mut chunks = vec![v & 63];
    let mut x = v >> 6;
    while x > 0 {
        chunks.push(x & 63);
        x >>= 6;
    }
    for (k, c) in chunks.iter().rev().enumerate() {
        let mut b = *c as u8;
        if k + 1 < chunks.len() {
            b |= 64;
        }
        if k == 0 && first {
            b |= 128;
        }
        out.push(b);
    }
}

pub fn encode_exception_table(e: &[ExcEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for x in e {
        write_be_varint(&mut out, x.start, true);
        write_be_varint(&mut out, x.end - x.start, false);
        write_be_varint(&mut out, x.target, false);
        write_be_varint(&mut out, (x.depth << 1) | x.lasti as u32, false);
    }
    out
}

// ------------------------------------------------------------------ location table

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub lineno: Option<u32>,
    pub end_lineno: Option<u32>,
    pub col: Option<u32>,
    pub end_col: Option<u32>,
}

/// One entry of the location table: `len` code units sharing `pos`. `line` is the
/// running line number (also for entries without location, used by co_lines()).
#[derive(Clone, Copy, Debug)]
pub struct LocEntry {
    pub len: u32,
    pub pos: Position,
}

fn read_le_varint(t: &[u8], pos: &mut usize) -> Option<u32> {
    let mut b = *t.get(*pos)?;
    *pos += 1;
    let mut v = (b & 63) as u32;
    let mut shift = 6;
    while b & 64 != 0 {
        b = *t.get(*pos)?;
        *pos += 1;
        if shift < 32 {
            v |= ((b & 63) as u32) << shift;
        }
        shift += 6;
    }
    Some(v)
}

fn read_signed_varint(t: &[u8], pos: &mut usize) -> Option<i64> {
    let u = read_le_varint(t, pos)?;
    Some(if u & 1 != 0 { -((u >> 1) as i64) } else { (u >> 1) as i64 })
}

/// Decode co_linetable into entries. Stops (returns what it has) at malformed data.
pub fn decode_linetable_entries(t: &[u8], firstlineno: u32) -> Vec<LocEntry> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut line = firstlineno as i64;
    while pos < t.len() {
        let b = t[pos];
        pos += 1;
        if b & 128 == 0 {
            break;
        }
        let code = (b >> 3) & 15;
        let len = (b & 7) as u32 + 1;
        let p = match code {
            0..=9 => {
                let Some(&s) = t.get(pos) else { break };
                pos += 1;
                let col = code as u32 * 8 + ((s >> 4) & 7) as u32;
                let ln = line.max(0) as u32;
                Position { lineno: Some(ln), end_lineno: Some(ln), col: Some(col), end_col: Some(col + (s & 15) as u32) }
            }
            10..=12 => {
                line += (code - 10) as i64;
                let (Some(&c1), Some(&c2)) = (t.get(pos), t.get(pos + 1)) else { break };
                pos += 2;
                let ln = line.max(0) as u32;
                Position { lineno: Some(ln), end_lineno: Some(ln), col: Some(c1 as u32), end_col: Some(c2 as u32) }
            }
            13 => {
                let Some(d) = read_signed_varint(t, &mut pos) else { break };
                line += d;
                let ln = line.max(0) as u32;
                Position { lineno: Some(ln), end_lineno: Some(ln), col: None, end_col: None }
            }
            14 => {
                let Some(d) = read_signed_varint(t, &mut pos) else { break };
                line += d;
                let Some(el) = read_le_varint(t, &mut pos) else { break };
                let Some(c) = read_le_varint(t, &mut pos) else { break };
                let Some(ec) = read_le_varint(t, &mut pos) else { break };
                let ln = line.max(0) as u32;
                Position {
                    lineno: Some(ln),
                    end_lineno: Some(ln + el),
                    col: if c == 0 { None } else { Some(c - 1) },
                    end_col: if ec == 0 { None } else { Some(ec - 1) },
                }
            }
            _ => Position::default(),
        };
        out.push(LocEntry { len, pos: p });
    }
    out
}

/// Per-code-unit positions (`ncodeunits` entries; units past the table get no location).
pub fn decode_linetable(t: &[u8], firstlineno: u32, ncodeunits: usize) -> Vec<Position> {
    let mut v = Vec::with_capacity(ncodeunits);
    for e in decode_linetable_entries(t, firstlineno) {
        for _ in 0..e.len {
            if v.len() < ncodeunits {
                v.push(e.pos);
            }
        }
    }
    v.resize(ncodeunits, Position::default());
    v
}

fn write_le_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 64 {
        out.push(64 | (v & 63) as u8);
        v >>= 6;
    }
    out.push(v as u8);
}

fn write_signed_varint(out: &mut Vec<u8>, v: i64) {
    let u = if v < 0 { ((-v as u64) << 1) | 1 } else { (v as u64) << 1 };
    write_le_varint(out, u as u32);
}

/// Encode per-instruction positions (`(length in code units, position)`) as co_linetable.
pub fn encode_linetable(items: &[(u32, Position)], firstlineno: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut line = firstlineno as i64;
    for &(len, p) in items {
        let mut rest = len;
        while rest > 0 {
            let n = rest.min(8);
            rest -= n;
            let head = |code: u8| 0x80 | (code << 3) | (n as u8 - 1);
            match p.lineno {
                None => out.push(head(15)),
                Some(l) => {
                    let delta = l as i64 - line;
                    line = l as i64;
                    let same_line = p.end_lineno == Some(l);
                    match (p.col, p.end_col) {
                        (Some(c), Some(ec)) if same_line && delta == 0 && c < 80 && ec >= c && ec - c < 16 => {
                            out.push(head((c / 8) as u8));
                            out.push((((c % 8) << 4) | (ec - c)) as u8);
                        }
                        (Some(c), Some(ec)) if same_line && (0..3).contains(&delta) && c < 128 && ec < 128 => {
                            out.push(head(10 + delta as u8));
                            out.push(c as u8);
                            out.push(ec as u8);
                        }
                        (None, None) if same_line => {
                            out.push(head(13));
                            write_signed_varint(&mut out, delta);
                        }
                        _ => {
                            out.push(head(14));
                            write_signed_varint(&mut out, delta);
                            let el = p.end_lineno.unwrap_or(l).saturating_sub(l);
                            write_le_varint(&mut out, el);
                            write_le_varint(&mut out, p.col.map_or(0, |c| c + 1));
                            write_le_varint(&mut out, p.end_col.map_or(0, |c| c + 1));
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exc_table_roundtrip() {
        let e = vec![
            ExcEntry { start: 2, end: 10, target: 40, depth: 0, lasti: false },
            ExcEntry { start: 10, end: 300, target: 5000, depth: 3, lasti: true },
        ];
        let b = encode_exception_table(&e);
        assert_eq!(decode_exception_table(&b).unwrap(), e);
    }

    #[test]
    fn linetable_roundtrip() {
        let p = |l: u32, el: u32, c: Option<u32>, ec: Option<u32>| Position {
            lineno: Some(l),
            end_lineno: Some(el),
            col: c,
            end_col: ec,
        };
        let items = vec![
            (1, p(5, 5, Some(0), Some(0))),
            (3, p(6, 6, Some(4), Some(10))),
            (9, p(6, 6, Some(100), Some(120))),
            (2, p(3, 7, Some(1), Some(2))),
            (1, Position::default()),
            (2, p(9, 9, None, None)),
            (1, p(9, 9, Some(200), Some(300))),
        ];
        let t = encode_linetable(&items, 5);
        let total: u32 = items.iter().map(|x| x.0).sum();
        let d = decode_linetable(&t, 5, total as usize);
        let mut k = 0;
        for (len, pos) in &items {
            for _ in 0..*len {
                assert_eq!(d[k], *pos, "unit {}", k);
                k += 1;
            }
        }
    }
}
