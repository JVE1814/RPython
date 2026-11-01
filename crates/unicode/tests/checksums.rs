//! Reproduces the SHA-1 checksums of spec/Lib/test/test_unicodedata.py
//! (UnicodeMethodsTest.test_method_checksum and
//! BaseUnicodeFunctionsTest.test_function_checksum for both databases).

mod common;
use common::*;
use rpy_unicode::{self as u, Ucd};

fn b(x: bool) -> u32 {
    if x { '1' as u32 } else { '0' as u32 }
}

fn method_checksum(maxunicode: u32) -> String {
    let mut h = Sha1::new();
    let mut bytes = Vec::new();
    for c in 0..=maxunicode {
        let ch = vec![c];
        let s1: Vec<u32> = [c, 'a' as u32, 'b' as u32, 'c' as u32].to_vec();
        let s2: Vec<u32> = [c, 'A' as u32, 'B' as u32, 'C' as u32].to_vec();
        let s3: Vec<u32> = [c, '1' as u32, '2' as u32, '3' as u32].to_vec();
        let sp: Vec<u32> = [c, ' ' as u32, '\t' as u32].to_vec();
        let mut data: Vec<u32> = vec![
            b(isalnum(&ch)),
            b(isalpha(&ch)),
            b(isdecimal(&ch)),
            b(isdigit(&ch)),
            b(islower(&ch)),
            b(isnumeric(&ch)),
            b(isspace(&ch)),
            b(istitle(&ch)),
            b(isupper(&ch)),
            b(isalnum(&s1)),
            b(isalpha(&s1)),
            b(isdecimal(&s3)),
            b(isdigit(&s3)),
            b(islower(&s1)),
            b(isnumeric(&s3)),
            b(isspace(&sp)),
            b(istitle(&s1)),
            b(isupper(&s2)),
        ];
        data.extend(lower(&ch));
        data.extend(upper(&ch));
        data.extend(title(&ch));
        data.extend(lower(&s1));
        data.extend(upper(&s2));
        data.extend(title(&s1));
        data.extend(title(&s2));
        bytes.clear();
        encode(&data, &mut bytes);
        h.update(&bytes);
    }
    h.hexdigest()
}

fn function_checksum(db: Ucd, maxunicode: u32) -> String {
    let mut h = Sha1::new();
    for c in 0..=maxunicode {
        let data = format!(
            "{}{}{}{}{}{}{}{}{}{}",
            fmt_g(db.digit(c).map_or(-1.0, |v| v as f64), 12),
            fmt_g(db.numeric(c).unwrap_or(-1.0), 12),
            fmt_g(db.decimal(c).map_or(-1.0, |v| v as f64), 12),
            db.category(c),
            db.bidirectional(c),
            db.decomposition(c),
            db.mirrored(c) as u8,
            db.combining(c),
            db.east_asian_width(c),
            db.name(c).unwrap_or_default(),
        );
        h.update(data.as_bytes());
    }
    h.hexdigest()
}

#[test]
fn sha1_works() {
    let mut h = Sha1::new();
    h.update(b"abc");
    assert_eq!(h.hexdigest(), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(fmt_g(0.125, 12), "0.125");
    assert_eq!(fmt_g(1e20, 12), "1e+20");
    assert_eq!(fmt_g(1.0 / 3.0, 12), "0.333333333333");
    assert_eq!(fmt_g(-1.0, 12), "-1");
    assert_eq!(fmt_g(100000000.0, 12), "100000000");
    assert_eq!(fmt_g(1e12, 12), "1e+12");
    assert_eq!(fmt_g(-0.5, 12), "-0.5");
}

#[test]
fn method_checksum_quick() {
    assert_eq!(method_checksum(0xFFFF), "47a99fa654ef1f50e89d2e9697b7b041fccb5a05");
}

#[test]
fn method_checksum_full() {
    assert_eq!(method_checksum(0x10FFFF), "8b2615a9fc627676cbc0b6fac0191177df97ef5f");
}

#[test]
fn function_checksum_quick() {
    assert_eq!(function_checksum(Ucd::Current, 0xFFFF), "00b13fa975a60b1d3f490f1fc8c126ab24990c75");
}

#[test]
fn function_checksum_full() {
    assert_eq!(function_checksum(Ucd::Current, 0x10FFFF), "ebfc9dd281c2226998fd435744dd2e9321899beb");
}

#[test]
fn function_checksum_320_quick() {
    assert_eq!(function_checksum(Ucd::V3_2_0, 0xFFFF), "883824cb6c0ccf994e4451ebf281e2d6d479af47");
}

#[test]
fn function_checksum_320_full() {
    assert_eq!(function_checksum(Ucd::V3_2_0, 0x10FFFF), "68cd01e2c680b851c1fcab012efb5635b2229c2b");
}

#[allow(dead_code)]
fn unused() {
    let _ = u::UNIDATA_VERSION;
}
