//! Generator for the CJK multibyte codec tables.
//!
//! Usage: cargo run --release --manifest-path tools/gencjk/Cargo.toml -- CACHE [REPO] [--report]
//! (CACHE is filled by tools/gencjk/fetch.sh).  Writes
//! crates/runtime/src/modules/cjk/data_{cn,jp,kr,tw,hk}.rs.

mod parse;

use parse::Entry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type Map = BTreeMap<u32, Vec<u32>>;

fn to_map(es: &[Entry], len: u8, keep: impl Fn(u32) -> bool) -> Map {
    let mut m = Map::new();
    for e in es {
        if e.len == len && !e.cps.is_empty() && keep(e.code) {
            m.entry(e.code).or_insert_with(|| e.cps.clone());
        }
    }
    m
}

fn report_diff(name: &str, base: &Map, ext: &Map) {
    let mut differ = 0;
    let mut added = 0;
    let mut missing = Vec::new();
    for (k, v) in ext {
        match base.get(k) {
            Some(b) if b == v => {}
            Some(b) => {
                differ += 1;
                eprintln!("  {name}: {k:#06X} base {b:X?} ext {v:X?}");
            }
            None => added += 1,
        }
    }
    for k in base.keys() {
        if !ext.contains_key(k) {
            missing.push(*k);
        }
    }
    eprintln!("{name}: {} entries, {added} added, {differ} differ, {} missing {:X?}", ext.len(), missing.len(), &missing[..missing.len().min(20)]);
}

mod gen;

pub fn johab_analysis(s: &gen::Sets) {
    let mut from_ksx = Map::new();
    for (k, v) in &s.ksx1001 {
        if let Some(j) = gen::ksx_to_johab(*k) {
            from_ksx.insert(j, v.clone());
        }
    }
    let mut jamo = Vec::new();
    let mut bad = 0;
    let mut nonh = 0;
    for (k, v) in &s.johab {
        if *k < 0x8000 {
            continue;
        }
        let lead = k >> 8;
        if lead <= 0xD3 {
            match gen::johab_syllable(*k) {
                Some(u) if v == &[u] => {}
                Some(u) => {
                    bad += 1;
                    eprintln!("johab hangul mismatch {k:#06X}: {v:X?} vs {u:X}")
                }
                None => jamo.push((*k, v[0])),
            }
        } else {
            nonh += 1;
            if from_ksx.get(k) != Some(v) {
                eprintln!("johab non-hangul mismatch {k:#06X}: {v:X?} vs {:X?}", from_ksx.get(k));
            }
        }
    }
    let extra: Vec<_> = from_ksx.keys().filter(|k| !s.johab.contains_key(k)).collect();
    eprintln!("johab: {bad} bad, {nonh} non-hangul, jamo {} {:X?}, extra from ksx {:X?}", jamo.len(), jamo, extra);
}

pub fn hk_analysis(s: &gen::Sets) {
    let mut big5_rev: BTreeMap<Vec<u32>, u32> = BTreeMap::new();
    for (k, v) in &s.big5 {
        big5_rev.entry(v.clone()).or_insert(*k);
    }
    for (k, v) in &s.hkscs {
        let lead = k >> 8;
        if let Some(b) = s.big5.get(k) {
            if b != v && !(0xC6..=0xC8).contains(&lead) {
                eprintln!("hk differs outside C6-C8: {k:#06X} big5 {b:X?} hk {v:X?}");
            }
            if b == v && (0xC6..=0xC8).contains(&lead) {
                eprintln!("hk same inside C6-C8: {k:#06X} {v:X?}");
            }
        }
        if let Some(bc) = big5_rev.get(v) {
            if bc != k {
                eprintln!("hk char also in big5: {v:X?} hk {k:#06X} big5 {bc:#06X}");
            }
        }
    }
    let lo: Vec<_> = s.big5.keys().filter(|k| (0xC6A1..=0xC8FE).contains(*k)).collect();
    eprintln!("big5 entries in C6A1..C8FE: {} first {:X?} last {:X?}", lo.len(), lo.first(), lo.last());
    eprintln!("jisx0213 2004 additions: {:X?}", s.jisx0213_2004_add);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cache = PathBuf::from(args.get(1).expect("usage: gencjk CACHE [REPO] [--report]"));
    let repo = args.get(2).filter(|a| !a.starts_with("--")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let report = args.iter().any(|a| a == "--report");
    let f = |n: &str| -> PathBuf { cache.join(n) };
    let t = |n: &str| parse::txt(&f(n));

    let sets = gen::Sets::load(&cache, t, &|p: &Path| parse::glibc(p), &|p: &Path| parse::xml(p), report);
    if report {
        report_diff("gbk/gb2312", &sets.gb2312, &sets.gbk);
        report_diff("gb18030/gbk", &sets.gbk, &sets.gb18030_2);
        report_diff("cp932/jisx0208(sjis)", &sets.jisx0208_sjis(), &sets.cp932);
        report_diff("jisx0213_1/jisx0208", &sets.jisx0208, &sets.jisx0213_1);
        report_diff("cp949/ksx1001", &sets.ksx1001, &sets.cp949);
        report_diff("cp950/big5", &sets.big5, &sets.cp950);
        hk_analysis(&sets);
        johab_analysis(&sets);
        return;
    }
    gen::write_all(&sets, &repo);
}
