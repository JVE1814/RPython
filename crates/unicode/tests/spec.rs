//! Expectations transcribed from spec/Lib/test/test_unicodedata.py,
//! test_ucn.py, test_str.py and test_unicode_identifiers.py.

mod common;
use common::*;
use rpy_unicode::{self as u, Form, Lookup, Ucd};

const CUR: Ucd = Ucd::Current;
const OLD: Ucd = Ucd::V3_2_0;


fn both(f: impl Fn(Ucd, bool)) {
    f(CUR, false);
    f(OLD, true);
}

#[test]
fn test_name() {
    both(|db, old| {
        for c in [0u32, 0xA, 0x1F, 0x7F, 0x9F, 0xFFFE, 0xFFFF, 0x10FFFF] {
            assert_eq!(db.name(c), None);
        }
        let n = |c: u32| db.name(c);
        let some = |s: &str| Some(s.to_string());
        let maybe = |s: &str| if old { None } else { Some(s.to_string()) };
        assert_eq!(n(' ' as u32), some("SPACE"));
        assert_eq!(n('1' as u32), some("DIGIT ONE"));
        assert_eq!(n('A' as u32), some("LATIN CAPITAL LETTER A"));
        assert_eq!(n(0xA0), some("NO-BREAK SPACE"));
        assert_eq!(n(0x221), maybe("LATIN SMALL LETTER D WITH CURL"));
        assert_eq!(n(0x3400), some("CJK UNIFIED IDEOGRAPH-3400"));
        assert_eq!(n(0x9FA5), some("CJK UNIFIED IDEOGRAPH-9FA5"));
        assert_eq!(n(0xAC00), some("HANGUL SYLLABLE GA"));
        assert_eq!(n(0xD7A3), some("HANGUL SYLLABLE HIH"));
        assert_eq!(n(0xF900), some("CJK COMPATIBILITY IDEOGRAPH-F900"));
        assert_eq!(n(0xFA6A), some("CJK COMPATIBILITY IDEOGRAPH-FA6A"));
        assert_eq!(n(0xFBF9), some("ARABIC LIGATURE UIGHUR KIRGHIZ YEH WITH HAMZA ABOVE WITH ALEF MAKSURA ISOLATED FORM"));
        assert_eq!(n(0x13460), maybe("EGYPTIAN HIEROGLYPH-13460"));
        assert_eq!(n(0x143FA), maybe("EGYPTIAN HIEROGLYPH-143FA"));
        assert_eq!(n(0x17000), maybe("TANGUT IDEOGRAPH-17000"));
        assert_eq!(n(0x18B00), maybe("KHITAN SMALL SCRIPT CHARACTER-18B00"));
        assert_eq!(n(0x18CD5), maybe("KHITAN SMALL SCRIPT CHARACTER-18CD5"));
        assert_eq!(n(0x18CFF), maybe("KHITAN SMALL SCRIPT CHARACTER-18CFF"));
        assert_eq!(n(0x18D1E), maybe("TANGUT IDEOGRAPH-18D1E"));
        assert_eq!(n(0x1B170), maybe("NUSHU CHARACTER-1B170"));
        assert_eq!(n(0x1B2FB), maybe("NUSHU CHARACTER-1B2FB"));
        assert_eq!(n(0x1FBA8), maybe("BOX DRAWINGS LIGHT DIAGONAL UPPER CENTRE TO MIDDLE LEFT AND MIDDLE RIGHT TO LOWER CENTRE"));
        assert_eq!(n(0x2A6D6), some("CJK UNIFIED IDEOGRAPH-2A6D6"));
        assert_eq!(n(0x2FA1D), some("CJK COMPATIBILITY IDEOGRAPH-2FA1D"));
        assert_eq!(n(0x33479), maybe("CJK UNIFIED IDEOGRAPH-33479"));
        // no names in PUA
        for c in (0xE000..0xF8FF).chain(0xF0000..0xFFFFF).chain(0x100000..0x10FFFF) {
            assert_eq!(db.name(c), None);
        }
        for nonexistent in [
            "LATIN SMLL LETR A", "OPEN HANDS SIGHS", "DREGS", "HANDBUG",
            "MODIFIER LETTER CYRILLIC SMALL QUESTION MARK", "???",
            "CJK UNIFIED IDEOGRAPH-03400", "CJK UNIFIED IDEOGRAPH-020000",
            "CJK UNIFIED IDEOGRAPH-33FF", "CJK UNIFIED IDEOGRAPH-F900",
            "CJK UNIFIED IDEOGRAPH-13460", "CJK UNIFIED IDEOGRAPH-17000",
            "CJK UNIFIED IDEOGRAPH-18B00", "CJK UNIFIED IDEOGRAPH-1B170",
            "CJK COMPATIBILITY IDEOGRAPH-3400", "TANGUT IDEOGRAPH-3400", "HANGUL SYLLABLE AC00",
        ] {
            assert_eq!(db.lookup(nonexistent), None, "{nonexistent}");
        }
        assert_eq!(db.lookup("GOTHIC LETTER FAIHU"), Some(Lookup::Char(0x10346)));
    });
}

#[test]
fn test_digit_numeric_decimal() {
    both(|db, old| {
        assert_eq!(db.digit('A' as u32), None);
        assert_eq!(db.digit('9' as u32), Some(9));
        assert_eq!(db.digit(0x215b), None);
        assert_eq!(db.digit(0x2468), Some(9));
        assert_eq!(db.digit(0x20000), None);
        assert_eq!(db.digit(0x1D7FD), Some(7));
        assert_eq!(db.digit(0x1fbf9), Some(9));
        assert_eq!(db.digit(0x16ac9), Some(9));
        assert_eq!(db.digit(0x1e4f9), Some(9));

        let nv = |v: f64| if old { None } else { Some(v) };
        assert_eq!(db.numeric('A' as u32), None);
        assert_eq!(db.numeric('9' as u32), Some(9.0));
        assert_eq!(db.numeric(0x215b), Some(0.125));
        assert_eq!(db.numeric(0x2468), Some(9.0));
        assert_eq!(db.numeric(0x20000), None);
        assert_eq!(db.numeric(0x1012A), nv(9000.0));
        assert_eq!(db.numeric(0x5793), if old { Some(1e20) } else { None });
        assert_eq!(db.numeric(0x07c0), nv(0.0));
        assert_eq!(db.numeric(0xa627), nv(7.0));
        assert_eq!(db.numeric(0x09f6), Some(if old { 3.0 } else { 3.0 / 16.0 }));
        assert_eq!(db.numeric(0x0b72), nv(0.25));
        assert_eq!(db.numeric(0x1ed3c), nv(0.5));
        assert_eq!(db.numeric(0x1fbf9), nv(9.0));
        assert_eq!(db.numeric(0x16ac9), nv(9.0));
        assert_eq!(db.numeric(0x1e4f9), nv(9.0));

        let dv = |v: u32| if old { None } else { Some(v) };
        assert_eq!(db.decimal('A' as u32), None);
        assert_eq!(db.decimal('9' as u32), Some(9));
        assert_eq!(db.decimal(0x215b), None);
        assert_eq!(db.decimal(0x2468), None);
        assert_eq!(db.decimal(0x20000), None);
        assert_eq!(db.decimal(0x1D7FD), Some(7));
        assert_eq!(db.decimal(0xb2), if old { Some(2) } else { None });
        assert_eq!(db.decimal(0x1369), if old { Some(1) } else { None });
        assert_eq!(db.decimal(0x07c0), dv(0));
        assert_eq!(db.decimal(0x1fbf9), dv(9));
        assert_eq!(db.decimal(0x16ac9), dv(9));
        assert_eq!(db.decimal(0x1e4f9), dv(9));

        // decimal/digit consistent with numeric
        let mut count = (0, 0);
        for c in 0..=0x10FFFF {
            if let Some(d) = db.decimal(c) {
                assert_eq!(Some(d as f64), db.numeric(c));
                count.0 += 1;
            }
            if let Some(d) = db.digit(c) {
                if db == CUR {
                    assert_eq!(Some(d as f64), db.numeric(c));
                }
                count.1 += 1;
            }
        }
        assert!(count.0 >= 10 && count.1 >= 10);
    });
}

#[test]
fn test_category_bidi() {
    both(|db, old| {
        let o = |a: &'static str, b: &'static str| if old { a } else { b };
        assert_eq!(db.category(0xFFFE), "Cn");
        assert_eq!(db.category('a' as u32), "Ll");
        assert_eq!(db.category('A' as u32), "Lu");
        assert_eq!(db.category(0x20000), "Lo");
        assert_eq!(db.category(0x1012A), o("Cn", "No"));
        assert_eq!(db.category(0xe01ef), o("Cn", "Mn"));
        assert_eq!(db.category(0x0374), o("Sk", "Lm"));
        assert_eq!(db.category(0x0b55), o("Cn", "Mn"));
        assert_eq!(db.category(0x3134a), o("Cn", "Lo"));
        assert_eq!(db.category(0x061d), o("Cn", "Po"));
        assert_eq!(db.category(0x2b738), o("Cn", "Lo"));
        assert_eq!(db.category(0x0cf3), o("Cn", "Mc"));
        assert_eq!(db.category(0x323af), o("Cn", "Lo"));

        assert_eq!(db.bidirectional(0xFFFE), o("", "BN"));
        assert_eq!(db.bidirectional(' ' as u32), "WS");
        assert_eq!(db.bidirectional('A' as u32), "L");
        assert_eq!(db.bidirectional(0x20000), "L");
        assert_eq!(db.bidirectional('+' as u32), o("ET", "ES"));
        assert_eq!(db.bidirectional(0x221), o("", "L"));
        assert_eq!(db.bidirectional(0xe01ef), o("", "NSM"));
        assert_eq!(db.bidirectional(0x0b55), o("", "NSM"));
        assert_eq!(db.bidirectional(0x3134a), o("", "L"));
        assert_eq!(db.bidirectional(0x061d), o("", "AL"));
        assert_eq!(db.bidirectional(0x2b738), o("", "L"));
        assert_eq!(db.bidirectional(0x0cf3), o("", "L"));
        assert_eq!(db.bidirectional(0x323af), o("", "L"));
        assert_eq!(db.bidirectional(0x0897), o("", "NSM"));
        assert_eq!(db.bidirectional(0x1fbef), o("", "ON"));
        assert_eq!(db.bidirectional(0x088f), o("", "AL"));
        assert_eq!(db.bidirectional(0x1fbfa), o("", "ON"));
        // unassigned
        assert_eq!(db.bidirectional(0x0378), o("", "L"));
        assert_eq!(db.bidirectional(0x077F), o("", "AL"));
        assert_eq!(db.bidirectional(0x20CF), o("", "ET"));
        assert_eq!(db.bidirectional(0x0590), o("", "R"));
        assert_eq!(db.bidirectional(0xFFFF), o("", "BN"));
        assert_eq!(db.bidirectional(0x1FFFE), o("", "BN"));
        assert_eq!(db.bidirectional(0x10D01), o("", "AL"));
    });
}

#[test]
fn test_decomposition_mirrored_combining() {
    both(|db, old| {
        let o = |a: &'static str, b: &'static str| if old { a } else { b };
        assert_eq!(db.decomposition(0xFFFE), "");
        assert_eq!(db.decomposition(0xbc), "<fraction> 0031 2044 0034");
        assert_eq!(db.decomposition(0x03f9), o("", "<compat> 03A3"));
        assert_eq!(db.decomposition(0xab69), o("", "<super> 028D"));
        assert_eq!(db.decomposition(0x11938), o("", "11935 11930"));
        assert_eq!(db.decomposition(0x1fbf9), o("", "<font> 0039"));
        assert_eq!(db.decomposition(0xa7f2), o("", "<super> 0043"));
        assert_eq!(db.decomposition(0x107ba), o("", "<super> 1DF1E"));
        assert_eq!(db.decomposition(0x1e06d), o("", "<super> 04B1"));
        assert_eq!(db.decomposition(0x1CCD6), o("", "<font> 0041"));
        assert_eq!(db.decomposition(0xA7F1), o("", "<super> 0053"));
        assert_eq!(db.decomposition(0xAC00), "1100 1161");
        assert_eq!(db.decomposition(0xD4DB), "1111 1171 11B6");
        assert_eq!(db.decomposition(0xC2F8), "110A 1161");
        assert_eq!(db.decomposition(0xD7A3), "1112 1175 11C2");

        assert!(!db.mirrored(0xFFFE));
        assert!(!db.mirrored('a' as u32));
        assert!(db.mirrored(0x2201));
        assert!(!db.mirrored(0x20000));
        assert_eq!(db.mirrored(0x0f3a), !old);
        assert_eq!(db.mirrored(0x1d7c3), !old);
        assert_eq!(db.mirrored(0x29a1), old);
        assert_eq!(db.mirrored(0x2e5c), !old);
        assert_eq!(db.mirrored(0x226D), !old);

        let c = |v: u8| if old { 0 } else { v };
        assert_eq!(db.combining(0xFFFE), 0);
        assert_eq!(db.combining('a' as u32), 0);
        assert_eq!(db.combining(0x20e1), 230);
        assert_eq!(db.combining(0x20000), 0);
        assert_eq!(db.combining(0x0350), c(230));
        assert_eq!(db.combining(0x1e94a), c(7));
        assert_eq!(db.combining(0x1abf), c(220));
        assert_eq!(db.combining(0x16ff1), c(6));
        assert_eq!(db.combining(0x0c3c), c(7));
        assert_eq!(db.combining(0x1e2ae), c(230));
        assert_eq!(db.combining(0x10efd), c(220));
        assert_eq!(db.combining(0x0897), c(230));
        assert_eq!(db.combining(0x1ACF), c(230));
    });
}

#[test]
fn test_east_asian_width() {
    both(|db, old| {
        let e = |c| db.east_asian_width(c);
        let o = |a: &'static str, b: &'static str| if old { a } else { b };
        assert_eq!(e(0x1e), "N");
        assert_eq!(e(0x20), "Na");
        assert_eq!(e(0xC894), "W");
        assert_eq!(e(0xFF66), "H");
        assert_eq!(e(0xFF1F), "F");
        assert_eq!(e(0x2010), "A");
        assert_eq!(e(0x20000), "W");
        assert_eq!(e(0x0350), o("N", "A"));
        assert_eq!(e(0xe01ef), o("N", "A"));
        assert_eq!(e(0x115a), o("N", "W"));
        for c in [0x231a, 0x2614, 0x1f19a, 0x1f991, 0x1f9c0, 0x32ff, 0x1fa95, 0x31bb, 0x3134a,
                  0x9ffd, 0x2b738, 0x323af, 0x2630, 0x1FAE9, 0x16FF2] {
            assert_eq!(e(c), o("N", "W"), "{c:X}");
        }
        for c in [0x0530, 0x0ecf, 0x10c6, 0x20fc, 0xaaca, 0x107bd, 0x115f2] {
            assert_eq!(e(c), "N");
            assert_eq!(db.name(c), None);
        }
        for c in [0x2A6E0, 0x2FA20, 0x3134B, 0x3FFFD, 0xFA6E, 0xFADA] {
            assert_eq!(e(c), "W", "{c:X}");
            assert_eq!(db.name(c), None);
        }
        for c in [0xE000, 0xF800, 0xF0000, 0xFFFEE, 0x100000, 0x10FFF0] {
            assert_eq!(e(c), "A");
            assert_eq!(db.name(c), None);
        }
    });
}

fn check_norm(db: Ucd, old: bool, input: &str, expected: [&str; 4]) {
    let x = s(input);
    let forms = [Form::NFC, Form::NFD, Form::NFKC, Form::NFKD];
    for (f, e) in forms.iter().zip(expected) {
        let r = db.normalize(*f, &x);
        assert_eq!(r, s(e), "{old} {f:?} {input:?}");
        assert_eq!(db.is_normalized(*f, &x), x == s(e), "{old} {f:?} {input:?}");
    }
}

#[test]
fn test_normalization() {
    both(|db, old| {
        let c1 = |i: &str, e: &str| check_norm(db, old, i, [e, e, e, e]);
        let c4 = |i: &str, e: [&str; 4]| check_norm(db, old, i, e);
        let c2 = |i: &str, a: &str, b: &str| check_norm(db, old, i, [a, b, a, b]);
        let o1 = |i: &str, e4: [&str; 4]| if old { c1(i, i) } else { c4(i, e4) };
        c1("", "");
        c1("A", "A");
        c1(" ", " ");
        c1("\u{10ffff}", "\u{10ffff}");
        c1("abc", "abc");
        c1("\u{0340}", "\u{0300}");
        c1("\u{0300}", "\u{0300}");
        c1("\u{2fa1d}", "\u{2a600}");
        c1("\u{2a600}", "\u{2a600}");
        c1("\u{0344}", "\u{0308}\u{0301}");
        c1("\u{0308}\u{0301}", "\u{0308}\u{0301}");
        c1("\u{1d1bc}", "\u{1d1ba}\u{1d165}");
        c1("\u{1d1ba}\u{1d165}", "\u{1d1ba}\u{1d165}");
        c1("\u{fb2c}", "\u{05e9}\u{05bc}\u{05c1}");
        c1("\u{05e9}\u{05bc}\u{05c1}", "\u{05e9}\u{05bc}\u{05c1}");
        c1("\u{1d1c0}", "\u{1d1ba}\u{1d165}\u{1d16f}");
        c1("\u{1d1ba}\u{1d165}\u{1d16f}", "\u{1d1ba}\u{1d165}\u{1d16f}");
        c4("\u{a0}", ["\u{a0}", "\u{a0}", " ", " "]);
        c4("\u{2003}", ["\u{2003}", "\u{2003}", " ", " "]);
        c4("\u{1d7ff}", ["\u{1d7ff}", "\u{1d7ff}", "9", "9"]);
        c4("\u{a8}", ["\u{a8}", "\u{a8}", " \u{0308}", " \u{0308}"]);
        c1(" \u{0308}", " \u{0308}");
        c2("\u{c0}", "\u{c0}", "A\u{0300}");
        c2("A\u{0300}", "\u{c0}", "A\u{0300}");
        c2("\u{d7a3}", "\u{d7a3}", "\u{1112}\u{1175}\u{11c2}");
        c2("\u{1112}\u{1175}\u{11c2}", "\u{d7a3}", "\u{1112}\u{1175}\u{11c2}");
        c4("\u{b4}", ["\u{b4}", "\u{b4}", " \u{0301}", " \u{0301}"]);
        c4("\u{1ffd}", ["\u{b4}", "\u{b4}", " \u{0301}", " \u{0301}"]);
        c1(" \u{0301}", " \u{0301}");
        c2("\u{c5}", "\u{c5}", "A\u{030a}");
        c2("\u{212b}", "\u{c5}", "A\u{030a}");
        c2("A\u{030a}", "\u{c5}", "A\u{030a}");
        c2("\u{1f71}", "\u{03ac}", "\u{03b1}\u{0301}");
        c2("\u{03ac}", "\u{03ac}", "\u{03b1}\u{0301}");
        c2("\u{03b1}\u{0301}", "\u{03ac}", "\u{03b1}\u{0301}");
        c4("\u{01c4}", ["\u{01c4}", "\u{01c4}", "D\u{017d}", "DZ\u{030c}"]);
        c2("D\u{017d}", "D\u{017d}", "DZ\u{030c}");
        c2("DZ\u{030c}", "D\u{017d}", "DZ\u{030c}");
        c4("\u{1fed}", ["\u{1fed}", "\u{a8}\u{0300}", " \u{0308}\u{0300}", " \u{0308}\u{0300}"]);
        c4("\u{a8}\u{0300}", ["\u{1fed}", "\u{a8}\u{0300}", " \u{0308}\u{0300}", " \u{0308}\u{0300}"]);
        c1(" \u{0308}\u{0300}", " \u{0308}\u{0300}");
        c4("\u{326e}", ["\u{326e}", "\u{326e}", "\u{ac00}", "\u{1100}\u{1161}"]);
        c4("\u{320e}", ["\u{320e}", "\u{320e}", "(\u{ac00})", "(\u{1100}\u{1161})"]);
        c2("(\u{ac00})", "(\u{ac00})", "(\u{1100}\u{1161})");
        c2("(\u{1100}\u{1161})", "(\u{ac00})", "(\u{1100}\u{1161})");
        c4("\u{0385}", ["\u{0385}", "\u{a8}\u{0301}", " \u{0308}\u{0301}", " \u{0308}\u{0301}"]);
        c4("\u{1fee}", ["\u{0385}", "\u{a8}\u{0301}", " \u{0308}\u{0301}", " \u{0308}\u{0301}"]);
        c4("\u{a8}\u{0301}", ["\u{0385}", "\u{a8}\u{0301}", " \u{0308}\u{0301}", " \u{0308}\u{0301}"]);
        c1(" \u{0308}\u{0301}", " \u{0308}\u{0301}");
        c4("\u{1fdf}", ["\u{1fdf}", "\u{1ffe}\u{0342}", " \u{0314}\u{0342}", " \u{0314}\u{0342}"]);
        c4("\u{1ffe}\u{0342}", ["\u{1fdf}", "\u{1ffe}\u{0342}", " \u{0314}\u{0342}", " \u{0314}\u{0342}"]);
        c4("\u{1ffe}", ["\u{1ffe}", "\u{1ffe}", " \u{0314}", " \u{0314}"]);
        c1(" \u{0314}\u{0342}", " \u{0314}\u{0342}");
        c4("\u{03d3}", ["\u{03d3}", "\u{03d2}\u{0301}", "\u{038e}", "\u{03a5}\u{0301}"]);
        c4("\u{03d2}\u{0301}", ["\u{03d3}", "\u{03d2}\u{0301}", "\u{038e}", "\u{03a5}\u{0301}"]);
        c2("\u{038e}", "\u{038e}", "\u{03a5}\u{0301}");
        c2("\u{1feb}", "\u{038e}", "\u{03a5}\u{0301}");
        c2("\u{03a5}\u{0301}", "\u{038e}", "\u{03a5}\u{0301}");
        c2("\u{0626}", "\u{0626}", "\u{064a}\u{0654}");
        c2("\u{064a}\u{0654}", "\u{0626}", "\u{064a}\u{0654}");
        for x in ["\u{fe89}", "\u{fe8a}", "\u{fe8b}", "\u{fe8c}"] {
            c4(x, [x, x, "\u{0626}", "\u{064a}\u{0654}"]);
        }
        c4("\u{fef9}", ["\u{fef9}", "\u{fef9}", "\u{0644}\u{0625}", "\u{0644}\u{0627}\u{0655}"]);
        c4("\u{fefa}", ["\u{fefa}", "\u{fefa}", "\u{0644}\u{0625}", "\u{0644}\u{0627}\u{0655}"]);
        c4("\u{fefb}", ["\u{fefb}", "\u{fefb}", "\u{0644}\u{0627}", "\u{0644}\u{0627}"]);
        c4("\u{fefc}", ["\u{fefc}", "\u{fefc}", "\u{0644}\u{0627}", "\u{0644}\u{0627}"]);
        c2("\u{0644}\u{0625}", "\u{0644}\u{0625}", "\u{0644}\u{0627}\u{0655}");
        c2("\u{0644}\u{0627}\u{0655}", "\u{0644}\u{0625}", "\u{0644}\u{0627}\u{0655}");
        c1("\u{0644}\u{0627}", "\u{0644}\u{0627}");
        o1("\u{327c}", ["\u{327c}", "\u{327c}", "\u{cc38}\u{ace0}", "\u{110e}\u{1161}\u{11b7}\u{1100}\u{1169}"]);
        c2("\u{cc38}\u{ace0}", "\u{cc38}\u{ace0}", "\u{110e}\u{1161}\u{11b7}\u{1100}\u{1169}");
        c2("\u{cc38}", "\u{cc38}", "\u{110e}\u{1161}\u{11b7}");
        c2("\u{110e}\u{1161}\u{11b7}\u{1100}\u{1169}", "\u{cc38}\u{ace0}", "\u{110e}\u{1161}\u{11b7}\u{1100}\u{1169}");
        c2("\u{110e}\u{1161}\u{11b7}\u{1100}", "\u{cc38}\u{1100}", "\u{110e}\u{1161}\u{11b7}\u{1100}");
        c2("\u{110e}\u{1161}\u{11b7}", "\u{cc38}", "\u{110e}\u{1161}\u{11b7}");
        c2("\u{110e}\u{1161}", "\u{cc28}", "\u{110e}\u{1161}");
        c1("\u{110e}", "\u{110e}");
        if old { c1("\u{11938}", "\u{11938}") } else { c2("\u{11938}", "\u{11938}", "\u{11935}\u{11930}") }
        c2("\u{11935}\u{11930}", "\u{11938}", "\u{11935}\u{11930}");
        o1("\u{321d}", ["\u{321d}", "\u{321d}", "(\u{c624}\u{c804})", "(\u{110b}\u{1169}\u{110c}\u{1165}\u{11ab})"]);
        c2("(\u{c624}\u{c804})", "(\u{c624}\u{c804})", "(\u{110b}\u{1169}\u{110c}\u{1165}\u{11ab})");
        c2("(\u{110b}\u{1169}\u{110c}\u{1165}\u{11ab})", "(\u{c624}\u{c804})", "(\u{110b}\u{1169}\u{110c}\u{1165}\u{11ab})");
        c1("\u{4d57}", "\u{4d57}");
        c1("\u{45d7}", "\u{45d7}");
        c1("\u{2f9bf}", if old { "\u{4d57}" } else { "\u{45d7}" });
        c1("\u{03a3}", "\u{03a3}");
        o1("\u{03f9}", ["\u{03f9}", "\u{03f9}", "\u{03a3}", "\u{03a3}"]);
        if old { c1("\u{1b06}", "\u{1b06}") } else { c2("\u{1b06}", "\u{1b06}", "\u{1b05}\u{1b35}") }
        o1("\u{1f213}", ["\u{1f213}", "\u{1f213}", "\u{30c7}", "\u{30c6}\u{3099}"]);
        c1("\u{fa2e}", if old { "\u{fa2e}" } else { "\u{90de}" });
        o1("\u{1fbf9}", ["\u{1fbf9}", "\u{1fbf9}", "9", "9"]);
        o1("\u{107ba}", ["\u{107ba}", "\u{107ba}", "\u{1df1e}", "\u{1df1e}"]);
        o1("\u{1e06d}", ["\u{1e06d}", "\u{1e06d}", "\u{04b1}", "\u{04b1}"]);
        o1("\u{1ccd6}", ["\u{1ccd6}", "\u{1ccd6}", "A", "A"]);

        // test_pr29
        for t in ["\u{0b47}\u{0300}\u{0b3e}", "\u{1100}\u{0300}\u{1161}", "Li\u{030d}t-s\u{1e73}\u{0301}",
                  "\u{092e}\u{093e}\u{0930}\u{094d}\u{0915} \u{091c}\u{093c}\u{0941}\u{0915}\u{0947}\u{0930}\u{092c}\u{0930}\u{094d}\u{0917}",
                  "\u{0915}\u{093f}\u{0930}\u{094d}\u{0917}\u{093f}\u{091c}\u{093c}\u{0938}\u{094d}\u{0924}\u{093e}\u{0928}"] {
            assert_eq!(db.normalize(Form::NFC, &s(t)), s(t));
        }
        // test_issue10254
        let a = "C\u{0338}".repeat(20) + "C\u{0327}";
        let b = "C\u{0338}".repeat(20) + "\u{C7}";
        assert_eq!(db.normalize(Form::NFC, &s(&a)), s(&b));
        // test_issue29456
        assert_eq!(db.normalize(Form::NFC, &s("\u{1100}\u{1176}\u{11a8}")), s("\u{1100}\u{1176}\u{11a8}"));
        assert_eq!(db.normalize(Form::NFC, &s("\u{1100}\u{1175}\u{11a7}")), s("\u{ae30}\u{11a7}"));
        assert_eq!(db.normalize(Form::NFC, &s("\u{1100}\u{1175}\u{11c3}")), s("\u{ae30}\u{11c3}"));
        // test_long_combining_mark_run / fast paths
        let payload = "a".to_string() + &"\u{0300}\u{0327}".repeat(32);
        let nfd = "a".to_string() + &"\u{0327}".repeat(32) + &"\u{0300}".repeat(32);
        let nfc = "\u{e0}".to_string() + &"\u{0327}".repeat(32) + &"\u{0300}".repeat(31);
        for (f, e) in [(Form::NFD, &nfd), (Form::NFKD, &nfd), (Form::NFC, &nfc), (Form::NFKC, &nfc)] {
            assert_eq!(db.normalize(f, &s(&payload)), s(e));
        }
        let short = "a".to_string() + &"\u{0300}\u{0327}".repeat(9) + "\u{0300}";
        let short_nfd = "a".to_string() + &"\u{0327}".repeat(9) + &"\u{0300}".repeat(10);
        let short_nfc = "\u{e0}".to_string() + &"\u{0327}".repeat(9) + &"\u{0300}".repeat(9);
        let long_sorted = "a".to_string() + &"\u{0327}".repeat(30) + &"\u{0300}".repeat(30);
        let long_sorted_nfc = "\u{e0}".to_string() + &"\u{0327}".repeat(30) + &"\u{0300}".repeat(29);
        for (f, i, e) in [(Form::NFD, &short, &short_nfd), (Form::NFKD, &short, &short_nfd),
                          (Form::NFC, &short, &short_nfc), (Form::NFKC, &short, &short_nfc),
                          (Form::NFD, &long_sorted, &long_sorted), (Form::NFKD, &long_sorted, &long_sorted),
                          (Form::NFC, &long_sorted, &long_sorted_nfc), (Form::NFKC, &long_sorted, &long_sorted_nfc)] {
            assert_eq!(db.normalize(f, &s(i)), s(e));
        }
        assert_eq!(db.normalize(Form::NFKC, &[]), Vec::<u32>::new());
        assert_eq!(Form::from_name("SPAM"), None);
    });
}

#[test]
fn test_xid() {
    for c in "S\u{0AD0}\u{0EC6}\u{17DC}\u{A015}\u{FE7B}".chars() {
        assert!(CUR.isxidstart(c as u32));
    }
    assert!(!CUR.isxidstart(' ' as u32));
    assert!(!CUR.isxidstart('0' as u32));
    for c in "S_0\u{00BA}\u{0640}\u{0710}\u{0B3E}\u{17D7}".chars() {
        assert!(CUR.isxidcontinue(c as u32));
    }
    assert!(!CUR.isxidcontinue(' ' as u32));
}

#[test]
fn test_grapheme_cluster_break() {
    let g = u::grapheme_cluster_break;
    let cases: &[(u32, &str)] = &[
        (0x20, "Other"), ('x' as u32, "Other"), (0x10FFFF, "Other"), (0xD, "CR"), (0xA, "LF"),
        (0, "Control"), (9, "Control"), (0x1F, "Control"), (0x7F, "Control"), (0x9F, "Control"),
        (0xE0001, "Control"), (0x300, "Extend"), (0x200C, "Extend"), (0xE01EF, "Extend"),
        (0x1159, "L"), (0x11F9, "T"), (0xD788, "LV"), (0xD7A3, "LVT"), (0x05BA, "Extend"),
        (0x20EF, "Extend"), (0x2064, "Control"), (0xAA4D, "SpacingMark"), (0x0816, "Extend"),
        (0xA97C, "L"), (0xD7C6, "V"), (0xD7FB, "T"), (0x093A, "Extend"), (0x11002, "SpacingMark"),
        (0xE0FFF, "Control"), (0x16F7E, "SpacingMark"), (0x1F1E6, "Regional_Indicator"),
        (0x1F1FF, "Regional_Indicator"), (0x180E, "Control"), (0x1A1B, "Extend"),
        (0x0E33, "SpacingMark"), (0x0EB3, "SpacingMark"), (0x1BCA3, "Control"), (0x1E8D6, "Extend"),
        (0x1163E, "SpacingMark"), (0x08E3, "Extend"), (0x11726, "SpacingMark"), (0x0600, "Prepend"),
        (0xE007F, "Extend"), (0x11CB4, "SpacingMark"), (0x200D, "ZWJ"), (0x11D46, "Prepend"),
        (0x11D47, "Extend"), (0x11A97, "SpacingMark"), (0x110CD, "Prepend"), (0x07FD, "Extend"),
        (0x11EF6, "SpacingMark"), (0x11A84, "Prepend"), (0x13438, "Control"), (0x1E2EF, "Extend"),
        (0x16F87, "SpacingMark"), (0x11941, "Prepend"), (0x16FE4, "Extend"), (0x11942, "SpacingMark"),
        (0x0891, "Prepend"), (0x1E2AE, "Extend"), (0x11F02, "Prepend"), (0x1343F, "Control"),
        (0x1E4EF, "Extend"), (0x11F3F, "SpacingMark"), (0x113D1, "Prepend"), (0x1E5EF, "Extend"),
        (0x1612C, "SpacingMark"), (0x16D63, "V"), (0x1AEB, "Extend"), (0x11B67, "SpacingMark"),
    ];
    for &(c, v) in cases {
        assert_eq!(g(c), v, "{c:X}");
    }
    let i = u::indic_conjunct_break;
    for &(c, v) in &[(0x20, "None"), ('x' as u32, "None"), (0x10FFFF, "None"), (0x094D, "Linker"),
                     (0x0D4D, "Linker"), (0x0915, "Consonant"), (0x0D3A, "Consonant"), (0x0300, "Extend"),
                     (0x1E94A, "Extend"), (0x034F, "Extend"), (0xE01EF, "Extend"), (0x1039, "Linker"),
                     (0x11F42, "Linker"), (0x1000, "Consonant"), (0x11F33, "Consonant"), (0x1E6F5, "Extend")] {
        assert_eq!(i(c), v, "{c:X}");
    }
    let e = u::extended_pictographic;
    for &(c, v) in &[(0x20, false), ('x' as u32, false), (0x10FFFF, false), (0xA9, true), (0x203C, true),
                     (0x1FAD6, true), (0x1FFFD, true), (0x2388, false), (0x1FA6D, false)] {
        assert_eq!(e(c), v, "{c:X}");
    }
}

fn graphemes(text: &str, start: isize, end: isize) -> Vec<String> {
    // emulates iter_graphemes(s, start, end) slice-index clamping
    let v = s(text);
    let n = v.len() as isize;
    let clamp = |x: isize| if x < 0 { (x + n).max(0) } else { x.min(n) } as usize;
    let (a, b) = (clamp(start), clamp(end));
    u::Graphemes::new(&v, a, b.max(a))
        .map(|(x, y)| v[x..y].iter().map(|&c| char::from_u32(c).unwrap()).collect())
        .collect()
}

#[test]
fn test_grapheme_break() {
    let g = |t: &str| graphemes(t, 0, isize::MAX);
    assert_eq!(g(""), Vec::<String>::new());
    assert_eq!(g("abcd"), ["a", "b", "c", "d"]);
    assert_eq!(graphemes("abcd", 1, isize::MAX), ["b", "c", "d"]);
    assert_eq!(graphemes("abcd", 1, 3), ["b", "c"]);
    assert_eq!(graphemes("abcd", -3, isize::MAX), ["b", "c", "d"]);
    assert_eq!(graphemes("abcd", 1, -1), ["b", "c"]);
    assert_eq!(graphemes("abcd", 3, 1), Vec::<String>::new());
    assert_eq!(graphemes("abcd", 5, isize::MAX), Vec::<String>::new());
    assert_eq!(graphemes("abcd", 0, 5), ["a", "b", "c", "d"]);
    assert_eq!(graphemes("abcd", -5, isize::MAX), ["a", "b", "c", "d"]);
    assert_eq!(graphemes("abcd", 0, -5), Vec::<String>::new());
    assert_eq!(g("\r\n"), ["\r\n"]);
    assert_eq!(g("\r\u{0308}"), ["\r", "\u{0308}"]);
    assert_eq!(g("\n\u{0308}"), ["\n", "\u{0308}"]);
    assert_eq!(g("\0\u{0308}"), ["\0", "\u{0308}"]);
    assert_eq!(g("\u{06dd}\r"), ["\u{06dd}", "\r"]);
    assert_eq!(g("\u{06dd}\n"), ["\u{06dd}", "\n"]);
    assert_eq!(g("\u{06dd}\0"), ["\u{06dd}", "\0"]);
    for t in ["\u{1100}\u{1160}", "\u{1100}\u{AC00}", "\u{1100}\u{AC01}", "\u{AC00}\u{1160}", "\u{AC00}\u{11A8}",
              "\u{1160}\u{1160}", "\u{1160}\u{11A8}", "\u{AC01}\u{11A8}", "\u{11A8}\u{11A8}", "a\u{0300}",
              "a\u{200D}", "\u{0905}\u{0903}", "\u{06dd}\u{0661}", "\u{0915}\u{094d}\u{0924}",
              "\u{0915}\u{094D}\u{094D}\u{0924}", "\u{0915}\u{094D}\u{0924}\u{094D}\u{092F}",
              "\u{1F9D1}\u{1F3FE}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F48B}\u{200D}\u{1F9D1}\u{1F3FC}"] {
        assert_eq!(g(t), [t]);
    }
    assert_eq!(g("\u{1F1FA}\u{1F1E6}\u{1F1FA}\u{1F1F3}"), ["\u{1F1FA}\u{1F1E6}", "\u{1F1FA}\u{1F1F3}"]);
    assert_eq!(g("a\u{1F1FA}\u{1F1E6}\u{1F1FA}\u{1F1F3}"), ["a", "\u{1F1FA}\u{1F1E6}", "\u{1F1FA}\u{1F1F3}"]);
    let segs: Vec<_> = u::Graphemes::new(&s("spa\u{0300}m"), 0, 5).collect();
    assert_eq!(segs, [(0, 1), (1, 2), (2, 4), (4, 5)]);
}

#[test]
fn test_block() {
    let cases: &[(u32, &str)] = &[
        (0, "Basic Latin"), (0x41, "Basic Latin"), (0x7F, "Basic Latin"), (0x80, "Latin-1 Supplement"),
        (0xFF, "Latin-1 Supplement"), (0x1159, "Hangul Jamo"), (0x11F9, "Hangul Jamo"),
        (0xD788, "Hangul Syllables"), (0xD7A3, "Hangul Syllables"), (0x05BA, "Hebrew"),
        (0x20EF, "Combining Diacritical Marks for Symbols"), (0x2064, "General Punctuation"),
        (0xAA4D, "Cham"), (0x0816, "Samaritan"), (0xA97C, "Hangul Jamo Extended-A"),
        (0xD7C6, "Hangul Jamo Extended-B"), (0xD7FB, "Hangul Jamo Extended-B"), (0x093A, "Devanagari"),
        (0x11002, "Brahmi"), (0xE0FFF, "No_Block"), (0x16F7E, "Miao"),
        (0x1F1E6, "Enclosed Alphanumeric Supplement"), (0x1F1FF, "Enclosed Alphanumeric Supplement"),
        (0x180E, "Mongolian"), (0x1A1B, "Buginese"), (0x0E33, "Thai"), (0x0EB3, "Lao"),
        (0x1BCA3, "Shorthand Format Controls"), (0x1E8D6, "Mende Kikakui"), (0x1163E, "Modi"),
        (0x08E3, "Arabic Extended-A"), (0x11726, "Ahom"), (0x0600, "Arabic"), (0xE007F, "Tags"),
        (0x11CB4, "Marchen"), (0x200D, "General Punctuation"), (0x11D46, "Masaram Gondi"),
        (0x11D47, "Masaram Gondi"), (0x11A97, "Soyombo"), (0x110CD, "Kaithi"), (0x07FD, "NKo"),
        (0x11EF6, "Makasar"), (0x11A84, "Soyombo"), (0x13438, "Egyptian Hieroglyph Format Controls"),
        (0x1E2EF, "Wancho"), (0x16F87, "Miao"), (0x11941, "Dives Akuru"),
        (0x16FE4, "Ideographic Symbols and Punctuation"), (0x11942, "Dives Akuru"),
        (0x0891, "Arabic Extended-B"), (0x1E2AE, "Toto"), (0x11F02, "Kawi"),
        (0x1343F, "Egyptian Hieroglyph Format Controls"), (0x1E4EF, "Nag Mundari"), (0x11F3F, "Kawi"),
        (0x113D1, "Tulu-Tigalari"), (0x1E5EF, "Ol Onal"), (0x1612C, "Gurung Khema"), (0x16D63, "Kirat Rai"),
        (0x1AEB, "Combining Diacritical Marks Extended"), (0x11B67, "Sharada Supplement"),
        (0x100000, "Supplementary Private Use Area-B"), (0x10FFFF, "Supplementary Private Use Area-B"),
    ];
    for &(c, b) in cases {
        assert_eq!(u::block(c), b, "{c:X}");
    }
}

#[test]
fn test_misc() {
    assert!(CUR.mirrored(0x0f3a));
    assert!(!OLD.mirrored(0x0f3a));
    assert_eq!(upper(&s("a")), s("A"));
    assert_eq!(upper(&s("\u{1d79}")), s("\u{a77d}"));
    assert_eq!(upper(&s(".")), s("."));
    assert_eq!(lower(&s("\u{1d79}")), s("\u{1d79}"));
    assert_eq!(title(&s("\u{01c4}")), s("\u{01c5}"));
    assert_eq!(title(&s("\u{01c5}")), s("\u{01c5}"));
    assert_eq!(title(&s("\u{01c6}")), s("\u{01c5}"));
    // test_bug_5828: only U+0000 maps to U+0000
    for c in 0..=0x10FFFF {
        let x = vec![c];
        let has0 = lower(&x).contains(&0) || upper(&x).contains(&0) || title(&x).contains(&0);
        assert_eq!(has0, c == 0);
    }
    // test_linebreak_7643
    for c in 0..=0x10FFFF {
        let want = [0x0a, 0x0b, 0x0c, 0x0d, 0x85, 0x1c, 0x1d, 0x1e, 0x2028, 0x2029].contains(&c);
        assert_eq!(u::is_linebreak(c), want, "{c:X}");
    }
    assert_eq!(CUR.unidata_version(), "17.0.0");
    assert_eq!(OLD.unidata_version(), "3.2.0");
}

// ------------------------------------------------------------- test_ucn.py

#[test]
fn test_ucn() {
    let chars = [
        "LATIN CAPITAL LETTER T", "LATIN SMALL LETTER H", "LATIN SMALL LETTER E", "SPACE",
        "LATIN SMALL LETTER R", "LATIN CAPITAL LETTER E", "LATIN SMALL LETTER D", "SPACE",
        "LATIN SMALL LETTER f", "LATIN CAPITAL LeTtEr o", "LATIN SMaLl LETTER x", "SPACE",
        "LATIN SMALL LETTER A", "LATIN SMALL LETTER T", "LATIN SMALL LETTER E", "SPACE",
        "LATIN SMALL LETTER T", "LATIN SMALL LETTER H", "LATIN SMALL LETTER E", "SpAcE",
        "LATIN SMALL LETTER S", "LATIN SMALL LETTER H", "LATIN small LETTER e", "LATIN small LETTER e",
        "LATIN SMALL LETTER P", "FULL STOP",
    ];
    let got: String = chars.iter().map(|n| char::from_u32(u::lookup_name(n).unwrap()).unwrap()).collect();
    assert_eq!(got, "The rEd fOx ate the sheep.");
    for c in 'a'..'z' {
        let n = format!("LATIN SMALL LETTER {}", c.to_ascii_uppercase());
        let cp = u::lookup_name(&n).unwrap();
        assert_eq!(u::name(cp).unwrap(), n);
    }
    let checks: &[(&str, u32)] = &[
        ("HANGUL SYLLABLE GA", 0xac00), ("HANGUL SYLLABLE GGWEOSS", 0xafe8), ("HANGUL SYLLABLE DOLS", 0xb3d0),
        ("HANGUL SYLLABLE RYAN", 0xb7b8), ("HANGUL SYLLABLE MWIK", 0xbba0), ("HANGUL SYLLABLE BBWAEM", 0xbf88),
        ("HANGUL SYLLABLE SSEOL", 0xc370), ("HANGUL SYLLABLE YI", 0xc758), ("HANGUL SYLLABLE JJYOSS", 0xcb40),
        ("HANGUL SYLLABLE KYEOLS", 0xcf28), ("HANGUL SYLLABLE PAN", 0xd310), ("HANGUL SYLLABLE HWEOK", 0xd6f8),
        ("HANGUL SYLLABLE HIH", 0xd7a3), ("haNGul SYllABle WAe", 0xc65c), ("HAngUL syLLabLE waE", 0xc65c),
        ("CJK UNIFIED IDEOGRAPH-3400", 0x3400), ("CJK UNIFIED IDEOGRAPH-4DB5", 0x4db5),
        ("CJK UNIFIED IDEOGRAPH-4E00", 0x4e00), ("CJK UNIFIED IDEOGRAPH-9FCB", 0x9fcb),
        ("CJK UNIFIED IDEOGRAPH-20000", 0x20000), ("CJK UNIFIED IDEOGRAPH-2A6D6", 0x2a6d6),
        ("CJK UNIFIED IDEOGRAPH-2A700", 0x2A700), ("CJK UNIFIED IDEOGRAPH-2B734", 0x2B734),
        ("CJK UNIFIED IDEOGRAPH-2B740", 0x2B740), ("CJK UNIFIED IDEOGRAPH-2B81D", 0x2B81D),
        ("CJK UNIFIED IDEOGRAPH-3134A", 0x3134A), ("cjK UniFIeD idEogRAph-3aBc", 0x3abc),
        ("CJk uNIfiEd IDeOGraPH-3AbC", 0x3abc), ("cjK UniFIeD idEogRAph-2aBcD", 0x2abcd),
        ("CJk uNIfiEd IDeOGraPH-2AbCd", 0x2abcd), ("TANGUT IDEOGRAPH-17000", 0x17000),
        ("TANGUT IDEOGRAPH-187FF", 0x187ff), ("TANGUT IDEOGRAPH-18D00", 0x18D00),
        ("TANGUT IDEOGRAPH-18D1E", 0x18d1e), ("tangut ideograph-18d1e", 0x18d1e),
        ("EGYPTIAN HIEROGLYPH-13460", 0x13460), ("EGYPTIAN HIEROGLYPH-143FA", 0x143fa),
        ("egyptian hieroglyph-143fa", 0x143fa), ("KHITAN SMALL SCRIPT CHARACTER-18B00", 0x18b00),
        ("KHITAN SMALL SCRIPT CHARACTER-18CD5", 0x18cd5), ("KHITAN SMALL SCRIPT CHARACTER-18CFF", 0x18cff),
        ("khitan small script character-18cff", 0x18cff), ("NUSHU CHARACTER-1B170", 0x1b170),
        ("NUSHU CHARACTER-1B2FB", 0x1b2fb), ("nushu character-1b2fb", 0x1b2fb),
        ("PILCROW SIGN", 0xb6), ("REPLACEMENT CHARACTER", 0xFFFD),
        ("HALFWIDTH KATAKANA SEMI-VOICED SOUND MARK", 0xFF9F), ("FULLWIDTH LATIN SMALL LETTER A", 0xFF41),
    ];
    for &(n, c) in checks {
        assert_eq!(u::lookup_name(n), Some(c), "{n}");
    }
    assert_eq!(u::name(0xd7a4), None);
    for c in 0..0x10000 {
        if let Some(n) = u::name(c) {
            assert_eq!(u::lookup_name(&n), Some(c));
        }
    }
    let aliases: &[(&str, u32)] = &[
        ("LATIN CAPITAL LETTER GHA", 0x01A2), ("LATIN SMALL LETTER GHA", 0x01A3),
        ("KANNADA LETTER LLLA", 0x0CDE), ("LAO LETTER FO FON", 0x0E9D), ("LAO LETTER FO FAY", 0x0E9F),
        ("LAO LETTER RO", 0x0EA3), ("LAO LETTER LO", 0x0EA5), ("TIBETAN MARK BKA- SHOG GI MGO RGYAN", 0x0FD0),
        ("YI SYLLABLE ITERATION MARK", 0xA015),
        ("PRESENTATION FORM FOR VERTICAL RIGHT WHITE LENTICULAR BRACKET", 0xFE18),
        ("BYZANTINE MUSICAL SYMBOL FTHORA SKLIRON CHROMA VASIS", 0x1D0C5),
    ];
    for &(a, c) in aliases {
        assert_eq!(u::lookup_name(a), Some(c));
        let n = u::name(c).unwrap();
        assert_ne!(n, a);
        assert_eq!(CUR.lookup(a), CUR.lookup(&n));
        assert_eq!(OLD.lookup(a), None);
    }
    for c in 0xf0000..0xf0fff {
        assert_eq!(u::name(c), None);
    }
    let seqs: &[(&str, &[u32])] = &[
        ("LATIN SMALL LETTER R WITH TILDE", &[0x72, 0x303]), ("TAMIL SYLLABLE SAI", &[0xBB8, 0xBC8]),
        ("TAMIL SYLLABLE MOO", &[0xBAE, 0xBCB]), ("TAMIL SYLLABLE NNOO", &[0xBA3, 0xBCB]),
        ("TAMIL CONSONANT KSS", &[0xB95, 0xBCD, 0xBB7, 0xBCD]),
    ];
    for &(n, v) in seqs {
        assert_eq!(CUR.lookup(n), Some(Lookup::Sequence(v)));
        assert_eq!(u::lookup_name(n), None);
        assert_eq!(OLD.lookup(n), None);
    }
    assert_eq!(u::lookup_name("unknown"), None);
    assert_eq!(u::lookup_name("blah"), None);
    assert_eq!(u::lookup_name(&"x".repeat(100000)), None);
    assert_eq!(u::lookup_name("GRINNING FACE"), Some(0x1F600));
    assert_eq!(u::lookup_name("SOFT HYPHEN"), Some(0xAD));
    // aliases for control characters
    assert_eq!(u::lookup_name("NULL"), Some(0));
    assert_eq!(u::lookup_name("line feed"), Some(10));
    assert_eq!(u::name(10), None);
}

// ------------------------------------------------------------- test_str.py

#[test]
fn test_str_methods() {
    for t in ["a", "Z", "_", "b0", "bc", "b_", "µ", "𝔘𝔫𝔦𝔠𝔬𝔡𝔢"] {
        assert!(u::is_identifier(&s(t)), "{t}");
    }
    for t in [" ", "[", "©", "0", ""] {
        assert!(!u::is_identifier(&s(t)), "{t}");
    }
    // test_unicode_identifiers
    for t in ["ä", "µ", "蟒", "x\u{E0100}"] {
        assert!(u::is_identifier(&s(t)), "{t}");
    }
    assert!(!u::is_identifier(&s("€")));

    assert!(isprintable(&s("")));
    assert!(isprintable(&s(" ")));
    assert!(isprintable(&s("abcdefg")));
    assert!(!isprintable(&s("abcdefg\n")));
    assert!(isprintable(&s("\u{0374}")));
    assert!(!isprintable(&s("\u{0378}")));
    assert!(!isprintable(&[0xd800]));
    assert!(isprintable(&s("\u{1F46F}")));
    assert!(!isprintable(&s("\u{E0020}")));
    for c in 0..=0x10FFFF {
        let cat = CUR.category(c);
        assert_eq!(u::is_printable(c), !(cat.starts_with('C') || cat.starts_with('Z')) || c == 0x20);
    }
    // test_surrogates
    for t in [&[0x61, 0xD800, 0x62, 0xDFFF][..], &[0x61, 0xDFFF, 0x62, 0xD800], &[0x61, 0xD800, 0x62, 0xDFFF, 0x61],
              &[0x61, 0xDFFF, 0x62, 0xD800, 0x61]] {
        assert!(islower(t));
        assert!(!isupper(t));
        assert!(!istitle(t));
    }
    for t in [&[0x41, 0xD800, 0x42, 0xDFFF][..], &[0x41, 0xDFFF, 0x42, 0xD800], &[0x41, 0xD800, 0x42, 0xDFFF, 0x41],
              &[0x41, 0xDFFF, 0x42, 0xD800, 0x41]] {
        assert!(!islower(t));
        assert!(isupper(t));
        assert!(istitle(t));
    }
    for t in [&[0xD800][..], &[0xDFFF], &[0xD800, 0xD800], &[0xDFFF, 0xDFFF]] {
        assert!(!islower(t) && !isupper(t) && !istitle(t));
        assert!(!isalpha(t) && !isalnum(t) && !isdigit(t) && !isspace(t) && !isdecimal(t));
        assert!(!isnumeric(t) && !u::is_identifier(t) && !isprintable(t));
    }
    for t in ["\u{1d7f6}", "\u{11066}", "\u{104A0}", "\u{1F107}"] {
        assert!(isnumeric(&s(t)));
    }

    let eq = |a: Vec<u32>, b: &str| assert_eq!(a, s(b));
    eq(lower(&s("\u{10427}")), "\u{1044F}");
    eq(lower(&s("\u{10427}\u{10427}")), "\u{1044F}\u{1044F}");
    eq(lower(&s("\u{10427}\u{1044F}")), "\u{1044F}\u{1044F}");
    eq(lower(&s("X\u{10427}x\u{1044F}")), "x\u{1044F}x\u{1044F}");
    eq(lower(&s("ﬁ")), "ﬁ");
    eq(lower(&s("\u{0130}")), "\u{0069}\u{0307}");
    eq(lower(&s("\u{03a3}")), "\u{03c3}");
    eq(lower(&s("\u{0345}\u{03a3}")), "\u{0345}\u{03c3}");
    eq(lower(&s("A\u{0345}\u{03a3}")), "a\u{0345}\u{03c2}");
    eq(lower(&s("A\u{0345}\u{03a3}a")), "a\u{0345}\u{03c3}a");
    eq(lower(&s("A\u{03a3}\u{0345}")), "a\u{03c2}\u{0345}");
    eq(lower(&s("\u{03a3}\u{0345} ")), "\u{03c3}\u{0345} ");
    eq(lower(&s("\u{8fffe}")), "\u{8fffe}");
    eq(lower(&s("\u{2177}")), "\u{2177}");

    eq(casefold(&s("hello")), "hello");
    eq(casefold(&s("hELlo")), "hello");
    eq(casefold(&s("ß")), "ss");
    eq(casefold(&s("ﬁ")), "fi");
    eq(casefold(&s("\u{03a3}")), "\u{03c3}");
    eq(casefold(&s("A\u{0345}\u{03a3}")), "a\u{03b9}\u{03c3}");
    eq(casefold(&s("\u{00b5}")), "\u{03bc}");

    eq(upper(&s("\u{1044F}")), "\u{10427}");
    eq(upper(&s("X\u{10427}x\u{1044F}")), "X\u{10427}X\u{10427}");
    eq(upper(&s("ﬁ")), "FI");
    eq(upper(&s("\u{0130}")), "\u{0130}");
    eq(upper(&s("\u{03a3}")), "\u{03a3}");
    eq(upper(&s("ß")), "SS");
    eq(upper(&s("\u{1fd2}")), "\u{0399}\u{0308}\u{0300}");
    eq(upper(&s("\u{8fffe}")), "\u{8fffe}");
    eq(upper(&s("\u{2177}")), "\u{2167}");

    eq(capitalize(&s("\u{1044F}")), "\u{10427}");
    eq(capitalize(&s("\u{1044F}\u{1044F}")), "\u{10427}\u{1044F}");
    eq(capitalize(&s("\u{10427}\u{1044F}")), "\u{10427}\u{1044F}");
    eq(capitalize(&s("\u{1044F}\u{10427}")), "\u{10427}\u{1044F}");
    eq(capitalize(&s("X\u{10427}x\u{1044F}")), "X\u{1044F}x\u{1044F}");
    eq(capitalize(&s("h\u{0130}")), "H\u{0069}\u{0307}");
    eq(capitalize(&s("\u{1fd2}\u{0130}")), "\u{0399}\u{0308}\u{0300}\u{0069}\u{0307}");
    eq(capitalize(&s("ﬁnnish")), "Finnish");
    eq(capitalize(&s("A\u{0345}\u{03a3}")), "A\u{0345}\u{03c2}");

    eq(title(&s("\u{1044F}")), "\u{10427}");
    eq(title(&s("\u{1044F}\u{1044F} \u{1044F}\u{1044F}")), "\u{10427}\u{1044F} \u{10427}\u{1044F}");
    eq(title(&s("X\u{10427}x\u{1044F} X\u{10427}x\u{1044F}")), "X\u{1044F}x\u{1044F} X\u{1044F}x\u{1044F}");
    eq(title(&s("ﬁNNISH")), "Finnish");
    eq(title(&s("A\u{03a3} \u{1fa1}xy")), "A\u{03c2} \u{1fa9}xy");
    eq(title(&s("A\u{03a3}A")), "A\u{03c3}a");

    eq(swapcase(&s("\u{1044F}")), "\u{10427}");
    eq(swapcase(&s("\u{10427}")), "\u{1044F}");
    eq(swapcase(&s("X\u{10427}x\u{1044F}")), "x\u{1044F}X\u{10427}");
    eq(swapcase(&s("ﬁ")), "FI");
    eq(swapcase(&s("\u{0130}")), "\u{0069}\u{0307}");
    eq(swapcase(&s("\u{03a3}")), "\u{03c3}");
    eq(swapcase(&s("\u{0345}\u{03a3}")), "\u{0399}\u{03c3}");
    eq(swapcase(&s("A\u{0345}\u{03a3}")), "a\u{0399}\u{03c2}");
    eq(swapcase(&s("A\u{0345}\u{03a3}a")), "a\u{0399}\u{03c3}A");

    assert!(isspace(&s(" \t\r\n")));
    assert!(!isspace(&s(" \t\r\na")));
    assert!(!isspace(&s("")));
}
