mod common;
use common::*;
use rpy_sre::flags::*;
use rpy_sre::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn long_alternation_repeat_no_stack_overflow() {
    let n = 1_000_000;
    let s: String = (0..n).map(|i| if i % 2 == 0 { 'a' } else { 'b' }).collect();
    let v = cp(&s);
    for p in [r"(?:a|b)*", r"(?:ab|ba|b)*", r"(a|b)*", r"((a)|b)*", r"(?:a|bb?)*?$", r"(?:(?:a|b)(?:b|a))*"] {
        let pat = c(p);
        let m = pat.match_at(&v, 0, usize::MAX).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(m.end(), n, "{p}");
    }
    // backtracking all the way back
    let pat = c(r"(?:a|b)*c");
    assert!(pat.match_at(&v, 0, usize::MAX).is_none());
    let pat = c(r"(?:ab|ba)*?c");
    assert!(pat.match_at(&v, 0, usize::MAX).is_none());
    let pat = c(r"(?:(a)|b){2,}+$");
    let m = pat.match_at(&v, 0, usize::MAX).unwrap();
    assert_eq!(m.spans[1], ((n - 2) as isize, (n - 1) as isize));
}

#[test]
fn code_unit_widths() {
    let p = c(r"(\w+)\s(\w+)");
    let s8: Vec<u8> = b"hello world".to_vec();
    let s16: Vec<u16> = "hello world".encode_utf16().collect();
    let s32 = cp("hello world");
    let a = p.search(&s8, 0, usize::MAX).unwrap();
    let b = p.search(&s16, 0, usize::MAX).unwrap();
    let cc = p.search(&s32, 0, usize::MAX).unwrap();
    assert_eq!(a, b);
    assert_eq!(b, cc);
    assert_eq!(a.spans[2], (6, 11));
    // latin-1 str subject with non-ASCII
    let l1: Vec<u8> = vec![b'x', 0xe9, 0xe8, b'y'];
    let m = c(r"\w+").search(&l1, 0, usize::MAX).unwrap();
    assert_eq!(m.spans[0], (0, 4));
    // literal prefix search on each width
    let p = c("needle");
    let mut hay = "x".repeat(1000);
    hay.push_str("needle");
    let h8: Vec<u8> = hay.bytes().collect();
    let h16: Vec<u16> = hay.encode_utf16().collect();
    assert_eq!(p.search(&h8, 0, usize::MAX).unwrap().spans[0], (1000, 1006));
    assert_eq!(p.search(&h16, 0, usize::MAX).unwrap().spans[0], (1000, 1006));
    // pattern char outside UCS-2 range never matches a u16 subject
    let p = c("\u{1F40D}");
    assert!(p.search(&h16, 0, usize::MAX).is_none());
}

static LATIN1: AtomicBool = AtomicBool::new(false);

fn loc_lower(b: u8) -> u8 {
    if LATIN1.load(Ordering::SeqCst) && (0xC0..=0xDE).contains(&b) && b != 0xD7 {
        b + 0x20
    } else {
        b.to_ascii_lowercase()
    }
}
fn loc_upper(b: u8) -> u8 {
    if LATIN1.load(Ordering::SeqCst) && (0xE0..=0xFE).contains(&b) && b != 0xF7 {
        b - 0x20
    } else {
        b.to_ascii_uppercase()
    }
}
fn loc_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric() || (LATIN1.load(Ordering::SeqCst) && b >= 0xC0 && b != 0xD7 && b != 0xF7)
}

#[test]
fn locale_runtime_switch() {
    let prov = UnicodeProvider {
        locale_lower: loc_lower,
        locale_upper: loc_upper,
        locale_is_alnum: loc_alnum,
        ..DEFAULT_PROVIDER
    };
    let comp = |p: &[u8], f: u32| compile_with(PatternSource::Bytes(p), f, &prov).unwrap();
    LATIN1.store(true, Ordering::SeqCst);
    let p1 = comp(b"\xc5\xe5", LOCALE | IGNORECASE);
    let p2 = comp(b"[a\xc5][a\xe5]", LOCALE | IGNORECASE);
    let p3 = comp(b"[az\xc5][az\xe5]", LOCALE | IGNORECASE);
    let p4 = comp(b"[^\xc5][^\xe5]", LOCALE | IGNORECASE);
    let p5 = comp(b"(?Li)\xc5", 0);
    let w = comp(br"\w", LOCALE);
    let m = |p: &Pattern, s: &[u8]| p.match_at(s, 0, usize::MAX).is_some();
    for p in [&p1, &p2, &p3] {
        assert!(m(p, b"\xc5\xe5"));
        assert!(m(p, b"\xe5\xe5"));
        assert!(m(p, b"\xc5\xc5"));
    }
    assert!(!m(&p4, b"\xe5\xc5"));
    assert!(!m(&p4, b"\xe5\xe5"));
    assert!(!m(&p4, b"\xc5\xc5"));
    assert!(m(&p5, b"\xe5"));
    assert!(m(&w, b"\xe5"));
    LATIN1.store(false, Ordering::SeqCst);
    for p in [&p1, &p2, &p3] {
        assert!(m(p, b"\xc5\xe5"));
        assert!(!m(p, b"\xe5\xe5"));
        assert!(!m(p, b"\xc5\xc5"));
    }
    assert!(m(&p4, b"\xe5\xc5"));
    assert!(!m(&p4, b"\xe5\xe5"));
    assert!(!m(&p4, b"\xc5\xc5"));
    assert!(!m(&p5, b"\xe5"));
    assert!(!m(&w, b"\xe5"));
}

#[test]
fn turkish_i_and_misc_case() {
    let mi = |p: &str, s: &str| re_match_f(p, s, IGNORECASE).is_some();
    // CPython: 'i' and 'ı' share uppercase 'I'; 'İ'.lower() starts with 'i'.
    assert!(mi("i", "\u{131}"));
    assert!(mi("I", "\u{131}"));
    assert!(mi("\u{130}", "i"));
    assert!(mi("\u{130}", "I"));
    assert!(mi("\u{131}", "I"));
    assert!(mi("\u{3c3}", "\u{3c2}"));
    assert!(mi("\u{3a3}", "\u{3c2}"));
    assert!(mi("\u{b5}", "\u{39c}"));
    assert!(mi("\u{b5}", "\u{3bc}"));
    assert!(!mi("a", "b"));
    // backreference ignore-case compares lowercase only
    assert!(re_match_f(r"(k)\1", "kK", IGNORECASE).is_some());
}

#[test]
fn pos_endpos_semantics() {
    let p = c("^a");
    let s = cp("ba");
    assert!(p.search(&s, 1, usize::MAX).is_none());
    let p = c("a$");
    assert!(p.search(&cp("abc"), 0, 1).is_some());
    let p = c(r"(?<=b)a");
    assert!(p.search(&cp("ba"), 1, usize::MAX).is_some());
    let p = c(r"\ba");
    assert!(p.search(&cp("ba"), 1, usize::MAX).is_none());
    // pos > endpos
    assert!(c("").search(&cp("abc"), 2, 1).is_none());
    assert!(c("").match_at(&cp("abc"), 5, 10).unwrap().spans[0] == (3, 3));
}

#[test]
fn empty_match_rules() {
    assert_eq!(sub("x*", "-", "abxd"), "-a-b--d-");
    assert_eq!(findall(r"\d*", "a12"), strs(&["", "12", ""]));
    assert_eq!(split("x*", "axbc"), somes(&["", "a", "", "b", "c", ""]));
    assert_eq!(re_match("(a)|(b)", "b").unwrap().lastindex(), Some(2));
    assert!(re_fullmatch(r"(?x)a\ b", "a b").is_some());
    assert!(re_match_f("\u{df}", "\u{1e9e}", IGNORECASE).is_some());
    assert!(re_match_f("\u{1e9e}", "\u{df}", IGNORECASE).is_some());
    // scanner.match
    let p = c(r"\w+|\s+");
    let v = cp("ab  cd");
    let mut sc = p.scanner(&v, 0, usize::MAX);
    assert_eq!(sc.match_().unwrap().spans[0], (0, 2));
    assert_eq!(sc.match_().unwrap().spans[0], (2, 4));
    assert_eq!(sc.match_().unwrap().spans[0], (4, 6));
    assert!(sc.match_().is_none());
}
