//! Character sets derived from the mapping files, and the Rust table writer.

use crate::parse::{self, Entry};
use crate::Map;
use std::fmt::Write as _;
use std::path::Path;

pub struct Sets {
    pub gb2312: Map,
    pub gbk: Map,
    pub gb18030_2: Map,
    pub gb18030_4: Vec<(u32, u32)>, // (linear index, code point), BMP only
    pub jisx0208: Map,
    pub jisx0212: Map,
    pub cp932: Map,
    pub jisx0213_1: Map,
    pub jisx0213_2: Map,
    pub jisx0213_2004_add: Map,
    pub ksx1001: Map,
    pub cp949: Map,
    pub johab: Map,
    pub big5: Map,
    pub cp950: Map,
    pub hkscs: Map,
}

fn two(es: &[Entry], keep: impl Fn(u32) -> bool) -> Map {
    crate::to_map(es, 2, keep)
}

/// EUC code -> Shift_JIS code for JIS X 0208 rows.
pub fn euc_to_sjis(code: u32) -> u32 {
    let c1 = (code >> 8) - 0x80;
    let c2 = (code & 0xFF) - 0x80;
    let row = c1 - 0x21;
    let lead = if row < 62 { 0x81 + row / 2 } else { 0xE0 + (row - 62) / 2 };
    let trail = if row % 2 == 0 {
        let t = c2 - 0x21 + 0x40;
        if t >= 0x7F { t + 1 } else { t }
    } else {
        c2 - 0x21 + 0x9F
    };
    lead << 8 | trail
}

impl Sets {
    pub fn load(
        cache: &Path,
        t: impl Fn(&str) -> Vec<Entry>,
        glibc: &dyn Fn(&Path) -> Vec<Entry>,
        xml: &dyn Fn(&Path) -> Vec<Entry>,
        report: bool,
    ) -> Sets {
        let euc_cn = t("EUC-CN.TXT");
        let cp936 = t("CP936.TXT");
        let gbx = xml(&cache.join("gb-18030-2000.xml"));
        let euc_jp = t("EUC-JP.TXT");
        let cp932 = t("CP932.TXT");
        let x0213 = t("EUC-JISX0213.TXT");
        let g0213 = glibc(&cache.join("glibc-EUC-JISX0213"));
        let euc_kr = t("EUC-KR.TXT");
        let cp949 = t("CP949.TXT");
        let johab = t("JOHAB.TXT");
        let big5 = t("BIG5.TXT");
        let cp950 = t("CP950.TXT");
        let hkscs = t("BIG5HKSCS-2004.TXT");

        let mut gb18030_4: Vec<(u32, u32)> = gbx
            .iter()
            .filter(|e| e.len == 4 && e.cps.len() == 1 && e.cps[0] < 0x10000)
            .map(|e| (parse::gb4_linear(e.code), e.cps[0]))
            .collect();
        gb18030_4.sort();
        let x0213_1 = two(&x0213, |c| c >= 0xA1A1);
        let g1: Map = two(&g0213, |c| c >= 0xA1A1);
        let mut add = Map::new();
        for (k, v) in &g1 {
            match x0213_1.get(k) {
                Some(o) if o == v => {}
                Some(o) => {
                    if report {
                        eprintln!("glibc jisx0213 differs at {k:#06X}: {o:X?} vs {v:X?}");
                    }
                }
                None => {
                    add.insert(*k, v.clone());
                }
            }
        }
        let big5m = two(&big5, |_| true).into_iter().filter(|(_, v)| v != &[0xFFFD]).collect();
        Sets {
            gb2312: two(&euc_cn, |_| true),
            gbk: two(&cp936, |_| true),
            gb18030_2: two(&gbx, |_| true),
            gb18030_4,
            jisx0208: two(&euc_jp, |c| c >= 0xA1A1),
            jisx0212: crate::to_map(&euc_jp, 3, |c| c >> 16 == 0x8F).into_iter().map(|(k, v)| (k & 0xFFFF, v)).collect(),
            cp932: two(&cp932, |_| true),
            jisx0213_1: x0213_1,
            jisx0213_2: {
                // The x0213.org :2000 table leaves 2-93-27 out (its mapping changed
                // in :2004); take the :2004 value from glibc.
                let mut p2: Map = crate::to_map(&x0213, 3, |c| c >> 16 == 0x8F).into_iter().map(|(k, v)| (k & 0xFFFF, v)).collect();
                for (k, v) in crate::to_map(&g0213, 3, |c| c >> 16 == 0x8F) {
                    p2.entry(k & 0xFFFF).or_insert(v);
                }
                p2
            },
            jisx0213_2004_add: add,
            ksx1001: two(&euc_kr, |_| true),
            cp949: two(&cp949, |_| true),
            johab: two(&johab, |_| true),
            big5: big5m,
            cp950: two(&cp950, |_| true),
            hkscs: two(&hkscs, |_| true),
        }
    }

    pub fn jisx0208_sjis(&self) -> Map {
        self.jisx0208.iter().map(|(k, v)| (euc_to_sjis(*k), v.clone())).collect()
    }
}

/// Entries of `ext` that are missing from or different in `base`.
fn overlay(base: &Map, ext: &Map) -> Map {
    ext.iter().filter(|(k, v)| base.get(k) != Some(v)).map(|(k, v)| (*k, v.clone())).collect()
}

/// Emit one two-byte table as a `Dbcs` static.
fn emit(out: &mut String, name: &str, doc: &str, m: &Map) {
    assert!(!m.is_empty(), "{name} is empty");
    let lo = (*m.keys().next().unwrap() >> 8) as u8;
    let hi = (*m.keys().last().unwrap() >> 8) as u8;
    let mut idx = Vec::new();
    let mut data: Vec<u16> = Vec::new();
    let mut wide: Vec<(u16, u32)> = Vec::new();
    for lead in lo as u32..=hi as u32 {
        let row: Vec<(u32, &Vec<u32>)> = m.range(lead << 8..(lead + 1) << 8).map(|(k, v)| (k & 0xFF, v)).collect();
        if row.is_empty() {
            idx.push((1u8, 0u8, 0u32));
            continue;
        }
        let tlo = row[0].0;
        let thi = row[row.len() - 1].0;
        idx.push((tlo as u8, thi as u8, data.len() as u32));
        let start = data.len();
        data.resize(start + (thi - tlo + 1) as usize, 0xFFFF);
        for (t, v) in row {
            let slot = start + (t - tlo) as usize;
            let code = (lead << 8 | t) as u16;
            match v.as_slice() {
                [c] if *c < 0xFFFE => data[slot] = *c as u16,
                [c] => {
                    data[slot] = 0xFFFE;
                    wide.push((code, *c));
                }
                [a, b] => {
                    assert!(*a < 0x10000 && *b < 0x10000);
                    data[slot] = 0xFFFE;
                    wide.push((code, 0x8000_0000 | a << 16 | b));
                }
                _ => panic!("{name}: bad entry {code:#x}"),
            }
        }
    }
    let _ = writeln!(out, "/// {doc} ({} mappings)", m.len());
    let _ = writeln!(out, "pub static {name}: Dbcs = Dbcs {{\n    lo: {lo:#04x},\n    idx: &[");
    for chunk in idx.chunks(4) {
        out.push_str("       ");
        for (a, b, c) in chunk {
            let _ = write!(out, " ({a:#04x}, {b:#04x}, {c}),");
        }
        out.push('\n');
    }
    // (mem1) the code-point slots (the bulk of each table) go to the cold read-only section
    out.push_str("    ],\n    data: crate::cold_slice!(u16, [\n");
    for chunk in data.chunks(16) {
        out.push_str("       ");
        for v in chunk {
            let _ = write!(out, " {v},");
        }
        out.push('\n');
    }
    out.push_str("    ]),\n    wide: &[");
    if !wide.is_empty() {
        out.push('\n');
        for chunk in wide.chunks(6) {
            out.push_str("       ");
            for (c, u) in chunk {
                let _ = write!(out, " ({c:#06x}, {u:#x}),");
            }
            out.push('\n');
        }
        out.push_str("    ");
    }
    out.push_str("],\n};\n\n");
}

fn header(region: &str) -> String {
    format!(
        "//! CJK codec tables ({region}).\n//!\n//! @generated by tools/gencjk from the public mapping tables listed in\n//! tools/gencjk/fetch.sh -- do not edit.\n\n#![cfg_attr(rustfmt, rustfmt_skip)]\n\nuse super::table::Dbcs;\n\n"
    )
}

fn write(repo: &Path, file: &str, text: String) {
    let p = repo.join("crates/runtime/src/modules/cjk").join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    eprintln!("wrote {}", p.display());
}

pub fn write_all(s: &Sets, repo: &Path) {
    // ---- Chinese (simplified)
    let mut cn = header("GB2312 / GBK / GB 18030");
    emit(&mut cn, "GB2312", "GB 2312 (EUC-CN codes)", &s.gb2312);
    let gbkext = overlay(&s.gb2312, &s.gbk);
    emit(&mut cn, "GBKEXT", "GBK (CP936) entries that are not plain GB 2312", &gbkext);
    let mut gbk_eff = s.gb2312.clone();
    gbk_eff.extend(gbkext.clone());
    assert!(s.gbk.len() == gbk_eff.len(), "GBK is not a superset of GB 2312");
    let gb18030ext = overlay(&s.gbk, &s.gb18030_2);
    assert!(s.gb18030_2.len() == s.gbk.len() + gb18030ext.len(), "GB 18030 overrides GBK");
    emit(&mut cn, "GB18030EXT", "GB 18030 two-byte codes beyond GBK", &gb18030ext);
    // Four-byte BMP mapping: monotone runs (linear index, first code point).
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut prev: Option<(u32, u32)> = None;
    for &(l, u) in &s.gb18030_4 {
        match prev {
            Some((pl, pu)) if pl + 1 == l && pu + 1 == u => {}
            Some((pl, pu)) => {
                assert!(l > pl && u > pu, "GB 18030 four-byte mapping is not monotone");
                runs.push((l, u));
            }
            None => runs.push((l, u)),
        }
        prev = Some((l, u));
    }
    let (last_l, last_u) = prev.unwrap();
    assert_eq!(last_u, 0xFFFF);
    let _ = writeln!(cn, "/// GB 18030 four-byte BMP mapping: runs of (linear index, first code point);\n/// the last run ends at linear index {last_l} (U+FFFF).");
    let _ = writeln!(cn, "pub static GB18030_RUNS: &[(u32, u32)] = &[");
    for chunk in runs.chunks(6) {
        cn.push_str("   ");
        for (l, u) in chunk {
            let _ = write!(cn, " ({l}, {u:#06x}),");
        }
        cn.push('\n');
    }
    let _ = writeln!(cn, "];\npub const GB18030_LAST_LINEAR: u32 = {last_l};");
    write(repo, "data_cn.rs", cn);

    // ---- Japanese
    let mut jp = header("JIS X 0208 / JIS X 0212 / CP932 / JIS X 0213");
    // 0xA1C0 stays U+005C here; the EUC/Shift_JIS codecs special-case it.
    let x0208 = s.jisx0208.clone();
    emit(&mut jp, "JISX0208", "JIS X 0208 (EUC-JP codes)", &x0208);
    emit(&mut jp, "JISX0212", "JIS X 0212 (EUC-JP codes without the 0x8F prefix)", &s.jisx0212);
    let sjis_0208: Map = x0208.iter().map(|(k, v)| (euc_to_sjis(*k), v.clone())).collect();
    let cp932ext = overlay(&sjis_0208, &s.cp932);
    emit(&mut jp, "CP932EXT", "CP932 two-byte codes that differ from or extend JIS X 0208 (Shift_JIS codes)", &cp932ext);
    let x0213_1 = s.jisx0213_1.clone();
    let mut x0213ext = overlay(&x0208, &x0213_1);
    for (k, v) in &s.jisx0213_2004_add {
        x0213ext.insert(*k, v.clone());
    }
    emit(&mut jp, "JISX0213_1", "JIS X 0213 plane 1 entries beyond JIS X 0208 (EUC codes, incl. the 2004 additions)", &x0213ext);
    emit(&mut jp, "JISX0213_2", "JIS X 0213 plane 2 (EUC codes without the 0x8F prefix)", &s.jisx0213_2);
    let _ = writeln!(jp, "/// Plane-1 codes added by JIS X 0213:2004 (invalid for the :2000 codecs).");
    let _ = write!(jp, "pub static JISX0213_2004_ADDED: &[u16] = &[");
    for k in s.jisx0213_2004_add.keys() {
        let _ = write!(jp, "{k:#06x}, ");
    }
    jp.push_str("];\n");
    write(repo, "data_jp.rs", jp);

    // ---- Korean
    let mut kr = header("KS X 1001 / CP949 / Johab");
    emit(&mut kr, "KSX1001", "KS X 1001 (EUC-KR codes)", &s.ksx1001);
    let cp949ext = overlay(&s.ksx1001, &s.cp949);
    assert_eq!(cp949ext.len() + s.ksx1001.len(), s.cp949.len());
    emit(&mut kr, "CP949EXT", "CP949 (Unified Hangul Code) codes beyond KS X 1001", &cp949ext);
    let mut jamo = Vec::new();
    for (k, v) in &s.johab {
        if *k >= 0x8000 && (k >> 8) <= 0xD3 && johab_syllable(*k).is_none() {
            jamo.push((*k, v[0]));
        }
    }
    let _ = writeln!(kr, "/// Johab codes of the stand-alone (compatibility) jamo.\npub static JOHAB_JAMO: &[(u16, u16)] = &[");
    for chunk in jamo.chunks(6) {
        kr.push_str("   ");
        for (k, u) in chunk {
            let _ = write!(kr, " ({k:#06x}, {u:#06x}),");
        }
        kr.push('\n');
    }
    kr.push_str("];\n");
    write(repo, "data_kr.rs", kr);

    // ---- Chinese (traditional)
    let mut tw = header("Big5 / CP950");
    emit(&mut tw, "BIG5", "Big5", &s.big5);
    let cp950ext = overlay(&s.big5, &s.cp950);
    emit(&mut tw, "CP950EXT", "CP950 codes that differ from or extend Big5", &cp950ext);
    write(repo, "data_tw.rs", tw);

    // ---- Hong Kong
    let mut hk = header("HKSCS-2004");
    let mut hkscs = s.hkscs.clone();
    // Code points with a combining mark (not listed in the one-to-one table).
    hkscs.insert(0x8862, vec![0x00CA, 0x0304]);
    hkscs.insert(0x8864, vec![0x00CA, 0x030C]);
    hkscs.insert(0x88A3, vec![0x00EA, 0x0304]);
    hkscs.insert(0x88A5, vec![0x00EA, 0x030C]);
    emit(&mut hk, "HKSCS", "HKSCS-2004 additions to Big5", &hkscs);
    write(repo, "data_hk.rs", hk);
}

/// KS X 1001 (EUC) code -> Johab code for the symbol and hanja rows.
pub fn ksx_to_johab(code: u32) -> Option<u32> {
    let r = (code >> 8) - 0xA0; // 1-based row
    let c = (code & 0xFF) - 0xA0; // 1-based column
    let lead = match r {
        1..=12 => 0xD9 + (r - 1) / 2,
        42..=93 => 0xE0 + (r - 42) / 2,
        _ => return None,
    };
    let first = if r <= 12 { r % 2 == 1 } else { r % 2 == 0 };
    let trail = if first {
        if c <= 78 { 0x30 + c } else { 0x90 + (c - 78) }
    } else {
        0xA0 + c
    };
    Some(lead << 8 | trail)
}

const JOHAB_MEDIAL: [i32; 32] = [
    -1, -1, -1, 0, 1, 2, 3, 4, -1, -1, 5, 6, 7, 8, 9, 10, -1, -1, 11, 12, 13, 14, 15, 16, -1, -1, 17, 18, 19, 20, -1, -1,
];

/// Johab code -> precomposed syllable (None for jamo-only / fill codes).
pub fn johab_syllable(code: u32) -> Option<u32> {
    let i = (code >> 10) & 31;
    let m = (code >> 5) & 31;
    let f = code & 31;
    if !(2..=20).contains(&i) {
        return None;
    }
    let mi = JOHAB_MEDIAL[m as usize];
    if mi < 0 {
        return None;
    }
    let fi = match f {
        1..=17 => f - 1,
        19..=29 => f - 2,
        _ => return None,
    };
    Some(0xAC00 + ((i - 2) * 21 + mi as u32) * 28 + fi)
}
