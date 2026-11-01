//! Readers for the mapping table formats.

use std::fs;
use std::path::Path;

/// One mapping line: the byte sequence (as a big-endian integer plus its
/// length) and the code point(s) it stands for (empty = undefined).
#[derive(Clone, Debug)]
pub struct Entry {
    pub code: u32,
    pub len: u8,
    pub cps: Vec<u32>,
}

fn hex(s: &str) -> u32 {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(s, 16).unwrap_or_else(|_| panic!("bad hex {s:?}"))
}

/// unicode.org style: `0xCODE<ws>0xUCS[<ws>0xUCS...]  # comment`.
pub fn txt(path: &Path) -> Vec<Entry> {
    let text = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let text = String::from_utf8_lossy(&text);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap();
        let mut it = line.split_whitespace();
        let Some(c) = it.next() else { continue };
        if !c.starts_with("0x") {
            continue;
        }
        let digits = c.len() - 2;
        let code = hex(c);
        let cps: Vec<u32> = it.map(|t| hex(t.trim_end_matches(','))).collect();
        out.push(Entry { code, len: (digits as u8 + 1) / 2, cps });
    }
    out
}

/// glibc charmap: `<UXXXX>[<UYYYY>] /xHH/xHH name`.
pub fn glibc(path: &Path) -> Vec<Entry> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    let mut in_map = false;
    for line in text.lines() {
        if line.starts_with("CHARMAP") {
            in_map = true;
            continue;
        }
        if line.starts_with("END CHARMAP") {
            break;
        }
        if !in_map || !line.starts_with("<U") {
            continue;
        }
        let mut it = line.split_whitespace();
        let u = it.next().unwrap();
        let b = it.next().unwrap();
        if u.contains("..") {
            continue;
        }
        let cps: Vec<u32> = u.split('>').filter(|s| !s.is_empty()).map(|s| hex(&s[2..])).collect();
        let bytes: Vec<u32> = b.split("/x").filter(|s| !s.is_empty()).map(hex).collect();
        let mut code = 0;
        for x in &bytes {
            code = code << 8 | x;
        }
        out.push(Entry { code, len: bytes.len() as u8, cps });
    }
    out
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')? + i;
    Some(&tag[i..j])
}

fn bytes_of(s: &str) -> (u32, u8) {
    let mut code = 0;
    let mut n = 0;
    for t in s.split_whitespace() {
        code = code << 8 | hex(t);
        n += 1;
    }
    (code, n)
}

/// GB 18030 four-byte sequence <-> linear index.
pub fn gb4_linear(code: u32) -> u32 {
    let b = [(code >> 24) & 0xFF, (code >> 16) & 0xFF, (code >> 8) & 0xFF, code & 0xFF];
    (((b[0] - 0x81) * 10 + (b[1] - 0x30)) * 126 + (b[2] - 0x81)) * 10 + (b[3] - 0x30)
}

/// Unicode CharacterMapping XML (gb-18030-2000.xml): `<a u= b=/>` entries
/// and `<range>` elements (four-byte runs).  Returned in file order.
pub fn xml(path: &Path) -> Vec<Entry> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = Vec::new();
    for raw in text.split('<').skip(1) {
        let tag = raw.split('>').next().unwrap();
        if let Some(rest) = tag.strip_prefix("a ") {
            let rest = format!(" {rest}");
            let u = attr(&rest, "u").unwrap();
            let (code, len) = bytes_of(attr(&rest, "b").unwrap());
            let cps = u.split_whitespace().map(hex).collect();
            out.push(Entry { code, len, cps });
        } else if let Some(rest) = tag.strip_prefix("range ") {
            let rest = format!(" {rest}");
            let uf = hex(attr(&rest, "uFirst").unwrap());
            let ul = hex(attr(&rest, "uLast").unwrap());
            let (bf, n) = bytes_of(attr(&rest, "bFirst").unwrap());
            assert_eq!(n, 4);
            let lin = gb4_linear(bf);
            for (k, u) in (uf..=ul).enumerate() {
                out.push(Entry { code: linear_to_gb4(lin + k as u32), len: 4, cps: vec![u] });
            }
        }
    }
    out
}

pub fn linear_to_gb4(mut l: u32) -> u32 {
    let b4 = l % 10 + 0x30;
    l /= 10;
    let b3 = l % 126 + 0x81;
    l /= 126;
    let b2 = l % 10 + 0x30;
    l /= 10;
    let b1 = l + 0x81;
    b1 << 24 | b2 << 16 | b3 << 8 | b4
}
