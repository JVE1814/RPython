#![allow(dead_code)]
//! Shared test helpers: UnicodeHooks backed by the rpy-unicode crate.

use rpy_parser::UnicodeHooks;

fn lookup(name: &str) -> Option<u32> {
    rpy_unicode::lookup_name(name)
}
fn xs(c: char) -> bool {
    rpy_unicode::is_xid_start(c as u32)
}
fn xc(c: char) -> bool {
    rpy_unicode::is_xid_continue(c as u32)
}
fn pr(c: char) -> bool {
    rpy_unicode::is_printable(c as u32)
}
fn nfkc(s: &str) -> String {
    let v: Vec<u32> = s.chars().map(|c| c as u32).collect();
    let n = rpy_unicode::normalize(rpy_unicode::Form::NFKC, &v);
    n.into_iter().filter_map(char::from_u32).collect()
}

pub fn hooks() -> UnicodeHooks {
    UnicodeHooks { lookup_name: lookup, is_xid_start: xs, is_xid_continue: xc, is_printable: pr, nfkc }
}

pub fn spec_path(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test").join(rel)
}
