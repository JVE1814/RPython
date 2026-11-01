//! Data-driven tests: NormalizationTest.txt (17.0.0 and 3.2.0),
//! DerivedName.txt, NamedSequences.txt / NameAliases.txt and
//! GraphemeBreakTest.txt, following the corresponding CPython tests.

mod common;
use common::*;
use rpy_unicode::{self as u, Form, Lookup, Ucd};
use std::collections::{HashMap, HashSet};

fn hexseq(s: &str) -> Vec<u32> {
    s.split_whitespace().map(|x| u32::from_str_radix(x, 16).unwrap()).collect()
}

fn run_normalization(text: &str, db: Ucd) -> usize {
    let n = |f, s: &Vec<u32>| db.normalize(f, s);
    let mut part = String::new();
    let mut part1: HashSet<u32> = HashSet::new();
    let mut count = 0;
    for line in text.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("@Part") {
            part = line.split_whitespace().next().unwrap().to_string();
            continue;
        }
        let f: Vec<Vec<u32>> = line.split(';').take(5).map(hexseq).collect();
        let (c1, c2, c3, c4, c5) = (&f[0], &f[1], &f[2], &f[3], &f[4]);
        use Form::*;
        assert!(*c2 == n(NFC, c1) && *c2 == n(NFC, c2) && *c2 == n(NFC, c3), "NFC {line}");
        assert!(*c4 == n(NFC, c4) && *c4 == n(NFC, c5), "NFC {line}");
        assert!(*c3 == n(NFD, c1) && *c3 == n(NFD, c2) && *c3 == n(NFD, c3), "NFD {line}");
        assert!(*c5 == n(NFD, c4) && *c5 == n(NFD, c5), "NFD {line}");
        for x in [c1, c2, c3, c4, c5] {
            assert_eq!(*c4, n(NFKC, x), "NFKC {line}");
            assert_eq!(*c5, n(NFKD, x), "NFKD {line}");
        }
        assert!(db.is_normalized(NFC, c2) && db.is_normalized(NFC, c4), "{line}");
        assert!(db.is_normalized(NFD, c3) && db.is_normalized(NFD, c5), "{line}");
        assert!(db.is_normalized(NFKC, c4) && db.is_normalized(NFKD, c5), "{line}");
        if part == "@Part1" {
            assert_eq!(c1.len(), 1);
            part1.insert(c1[0]);
        }
        count += 1;
    }
    for c in 0..=0x10FFFFu32 {
        if part1.contains(&c) {
            continue;
        }
        let x = vec![c];
        for f in [Form::NFC, Form::NFD, Form::NFKC, Form::NFKD] {
            assert_eq!(db.normalize(f, &x), x, "{c:04X}");
        }
    }
    count
}

#[test]
fn normalization_test_current() {
    let text = std::fs::read_to_string(data_path("data/17.0.0/NormalizationTest.txt")).unwrap();
    assert!(text.lines().next().unwrap().contains(u::UNIDATA_VERSION));
    assert!(run_normalization(&text, Ucd::Current) > 19000);
}

#[test]
fn normalization_test_3_2_0() {
    let text = std::fs::read_to_string(spec_path("Lib/test/NormalizationTest-3.2.0.txt")).unwrap();
    assert!(run_normalization(&text, Ucd::V3_2_0) > 15000);
}

#[test]
fn normalize_consistent() {
    // test_unicodedata.test_normalize_consistent, all code points, both DBs
    for db in [Ucd::Current, Ucd::V3_2_0] {
        for f in [Form::NFC, Form::NFD, Form::NFKC, Form::NFKD] {
            for c in 0..=0x10FFFFu32 {
                let x = vec![c];
                let norm = db.normalize(f, &x);
                assert_eq!(db.is_normalized(f, &x), norm == x, "{db:?} {f:?} {c:04X}");
                if norm != x {
                    assert_eq!(db.normalize(f, &norm), norm);
                    assert!(db.is_normalized(f, &norm));
                }
            }
        }
    }
}

#[test]
fn all_names() {
    // test_unicodedata.test_all_names with DerivedName.txt
    let text = std::fs::read_to_string(data_path("data/17.0.0/DerivedName.txt")).unwrap();
    let mut names: HashMap<u32, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (raw, name) = line.split_once("; ").unwrap();
        let raw = raw.trim();
        let (a, b) = match raw.split_once("..") {
            Some((a, b)) => (u32::from_str_radix(a, 16).unwrap(), u32::from_str_radix(b, 16).unwrap()),
            None => {
                let v = u32::from_str_radix(raw, 16).unwrap();
                (v, v)
            }
        };
        for cp in a..=b {
            let n = match name.strip_suffix('*') {
                Some(p) => format!("{p}{cp:04X}"),
                None => name.to_string(),
            };
            names.insert(cp, n);
        }
    }
    for cp in 0..=0x10FFFFu32 {
        assert_eq!(u::name(cp), names.get(&cp).cloned(), "{cp:04X}");
    }
}

#[test]
fn name_inverse_lookup() {
    for db in [Ucd::Current, Ucd::V3_2_0] {
        for cp in 0..=0x10FFFFu32 {
            if let Some(n) = db.name(cp) {
                assert_eq!(db.lookup(&n), Some(Lookup::Char(cp)), "{n}");
                assert_eq!(db.lookup(&n.to_lowercase()), Some(Lookup::Char(cp)), "{n}");
            }
        }
    }
}

#[test]
fn named_sequences_full() {
    let text = std::fs::read_to_string(data_path("data/17.0.0/NamedSequences.txt")).unwrap();
    let mut n = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, cps) = line.split_once(';').unwrap();
        let cps = hexseq(cps);
        match Ucd::Current.lookup(name) {
            Some(Lookup::Sequence(s)) => assert_eq!(s, &cps[..], "{name}"),
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(u::lookup_name(name), None, "\\N{{{name}}} must fail");
        assert_eq!(Ucd::V3_2_0.lookup(name), None);
        n += 1;
    }
    assert!(n > 400);
}

#[test]
fn aliases_full() {
    let text = std::fs::read_to_string(data_path("data/17.0.0/NameAliases.txt")).unwrap();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split(';').collect();
        let cp = u32::from_str_radix(f[0], 16).unwrap();
        assert_eq!(u::lookup_name(f[1]), Some(cp), "{}", f[1]);
        assert_eq!(Ucd::Current.lookup(f[1]), Some(Lookup::Char(cp)));
        assert_ne!(u::name(cp).as_deref(), Some(f[1]));
    }
}

#[test]
fn grapheme_break_test() {
    let text = std::fs::read_to_string(data_path("data/17.0.0/GraphemeBreakTest.txt")).unwrap();
    let mut n = 0;
    for line in text.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut input = Vec::new();
        let mut breaks = Vec::new();
        for field in line.replace('×', " ").split_whitespace() {
            if field == "÷" {
                breaks.push(input.len());
            } else {
                input.push(u32::from_str_radix(field, 16).unwrap());
            }
        }
        assert_eq!(*breaks.last().unwrap(), input.len());
        let got: Vec<(usize, usize)> = u::Graphemes::new(&input, 0, input.len()).collect();
        let want: Vec<(usize, usize)> = breaks.windows(2).map(|w| (w[0], w[1])).collect();
        assert_eq!(got, want, "{line}");
        for i in 1..breaks.len() - 1 {
            let got: Vec<(usize, usize)> = u::Graphemes::new(&input, breaks[i], input.len()).collect();
            assert_eq!(got, want[i..].to_vec(), "{line} from {i}");
        }
        n += 1;
    }
    assert!(n > 700);
}
