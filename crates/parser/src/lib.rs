//! `rpy-parser`: tokenizer, parser and AST for a Rust reimplementation of CPython 3.15.
//!
//! Entry points: [`parse`] / [`parse_bytes`] (source -> [`ast::Mod`]), [`tokenizer::TokenIter`]
//! (stdlib-`tokenize`-compatible token stream), [`ast::node_classes`] (AST class table).

pub mod ast;
pub mod dump;
pub mod error;
pub mod parser;
pub mod string;
pub mod token;
pub mod tokenizer;

pub use error::{ErrorKind, SyntaxError, Warning, WarningCategory};

/// Unicode database hooks. The parser crate has no Unicode tables of its own; the runtime
/// passes functions backed by the unicode crate. The defaults are approximations based on
/// Rust's `char` methods (no NFKC normalization, no `\N{...}` names).
#[derive(Clone, Copy)]
pub struct UnicodeHooks {
    /// Lookup for `\N{NAME}` escapes (also handles aliases / named sequences returning one cp).
    pub lookup_name: fn(&str) -> Option<u32>,
    pub is_xid_start: fn(char) -> bool,
    pub is_xid_continue: fn(char) -> bool,
    /// `str.isprintable()` for one character (used for error messages).
    pub is_printable: fn(char) -> bool,
    /// NFKC normalization (identifiers are NFKC-normalized).
    pub nfkc: fn(&str) -> String,
}

fn default_lookup_name(_: &str) -> Option<u32> {
    None
}
fn default_xid_start(c: char) -> bool {
    c.is_alphabetic()
}
fn default_xid_continue(c: char) -> bool {
    c.is_alphanumeric() || matches!(c as u32, 0x300..=0x36F | 0xE0100..=0xE01EF | 0x200C | 0x200D | 0xFE00..=0xFE0F) || c == '\u{B7}'
}
fn default_is_printable(c: char) -> bool {
    !(c.is_control() || (c.is_whitespace() && c != ' ') || matches!(c as u32, 0xAD | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064 | 0xFEFF))
}
fn default_nfkc(s: &str) -> String {
    s.to_string()
}

impl Default for UnicodeHooks {
    fn default() -> Self {
        UnicodeHooks {
            lookup_name: default_lookup_name,
            is_xid_start: default_xid_start,
            is_xid_continue: default_xid_continue,
            is_printable: default_is_printable,
            nfkc: default_nfkc,
        }
    }
}

impl std::fmt::Debug for UnicodeHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UnicodeHooks")
    }
}

pub use parser::{parse, parse_bytes, Mode, ParseOptions, ParseOutput};
