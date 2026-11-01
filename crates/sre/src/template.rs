//! Replacement templates for `re.sub` / `Match.expand`.

use crate::error::{CompileError, TemplateError};
use crate::parser::{py_repr, Source};
use crate::unicode::UnicodeProvider;
use crate::MAXGROUPS;

/// One element of a parsed replacement template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateItem {
    /// Literal code points (bytes values for bytes templates).
    Literal(Vec<u32>),
    /// Reference to a capture group (0 = whole match).
    Group(usize),
}

fn conv(e: CompileError) -> TemplateError {
    match e {
        CompileError::Pattern { msg, pos } => TemplateError::Pattern { msg, pos: pos.unwrap_or(0) },
        CompileError::Overflow(m) | CompileError::Value(m) => TemplateError::Pattern { msg: m, pos: 0 },
    }
}

fn is_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
}
fn is_oct(c: u32) -> bool {
    (0x30..=0x37).contains(&c)
}

pub(crate) fn parse_template_impl(
    repl: &[u32],
    is_bytes: bool,
    groups: usize,
    lookup: &dyn Fn(&[u32]) -> Option<usize>,
    prov: &UnicodeProvider,
) -> Result<Vec<TemplateItem>, TemplateError> {
    let mut s = Source::new(repl).map_err(conv)?;
    let mut result: Vec<TemplateItem> = Vec::new();
    let mut literal: Vec<u32> = Vec::new();
    let err = |s: &Source, msg: String, off: usize| TemplateError::Pattern {
        msg,
        pos: s.tell().saturating_sub(off),
    };
    let repr = |v: &[u32]| py_repr(v, is_bytes, prov);
    let is_ident = |v: &[u32]| {
        !v.is_empty()
            && !(is_bytes && v.iter().any(|&c| c >= 128))
            && (prov.is_identifier_start)(v[0])
            && v[1..].iter().all(|&c| (prov.is_identifier_continue)(c))
    };
    macro_rules! addgroup {
        ($index:expr, $off:expr) => {{
            let index: u128 = $index;
            if index > groups as u128 {
                return Err(err(&s, format!("invalid group reference {index}"), $off));
            }
            if !literal.is_empty() {
                result.push(TemplateItem::Literal(std::mem::take(&mut literal)));
            }
            result.push(TemplateItem::Group(index as usize));
        }};
    }
    loop {
        let Some(this) = s.get().map_err(conv)? else { break };
        if !this.esc {
            literal.push(this.c);
            continue;
        }
        let c = this.c;
        if c == 'g' as u32 {
            if !s.eat('<').map_err(conv)? {
                return Err(err(&s, "missing <".into(), 0));
            }
            let name = s.getuntil('>', "group name").map_err(conv)?;
            let off = name.len() + 1;
            if name.iter().all(|&c| is_digit(c)) {
                let mut v: u128 = 0;
                for &d in &name {
                    v = v.saturating_mul(10).saturating_add((d - 0x30) as u128);
                }
                if v >= MAXGROUPS as u128 {
                    return Err(err(&s, format!("invalid group reference {v}"), off));
                }
                addgroup!(v, off);
            } else if is_ident(&name) {
                match lookup(&name) {
                    Some(i) => addgroup!(i as u128, off),
                    None => {
                        return Err(TemplateError::Index(format!(
                            "unknown group name {}",
                            repr(&name)
                        )))
                    }
                }
            } else {
                return Err(err(&s, format!("bad character in group name {}", repr(&name)), off));
            }
        } else if c == '0' as u32 {
            let mut v: u32 = 0;
            for _ in 0..2 {
                match s.next {
                    Some(t) if !t.esc && is_oct(t.c) => {
                        v = v * 8 + (t.c - 0x30);
                        s.get().map_err(conv)?;
                    }
                    _ => break,
                }
            }
            literal.push(v & 0xff);
        } else if is_digit(c) {
            let mut digits = vec![c];
            let mut isoctal = false;
            if let Some(t) = s.next {
                if !t.esc && is_digit(t.c) {
                    s.get().map_err(conv)?;
                    digits.push(t.c);
                    if is_oct(c) && is_oct(t.c) {
                        if let Some(t2) = s.next {
                            if !t2.esc && is_oct(t2.c) {
                                s.get().map_err(conv)?;
                                digits.push(t2.c);
                                isoctal = true;
                                let v = digits.iter().fold(0u32, |a, &d| a * 8 + (d - 0x30));
                                if v > 0o377 {
                                    let txt: String = std::iter::once('\\')
                                        .chain(digits.iter().map(|&d| char::from_u32(d).unwrap()))
                                        .collect();
                                    return Err(err(
                                        &s,
                                        format!("octal escape value {txt} outside of range 0-0o377"),
                                        4,
                                    ));
                                }
                                literal.push(v);
                            }
                        }
                    }
                }
            }
            if !isoctal {
                let v = digits.iter().fold(0u128, |a, &d| a * 10 + (d - 0x30) as u128);
                addgroup!(v, digits.len());
            }
        } else {
            let v = match char::from_u32(c) {
                Some('a') => Some(7),
                Some('b') => Some(8),
                Some('f') => Some(12),
                Some('n') => Some(10),
                Some('r') => Some(13),
                Some('t') => Some(9),
                Some('v') => Some(11),
                Some('\\') => Some(92),
                _ => None,
            };
            match v {
                Some(v) => literal.push(v),
                None => {
                    if c < 128 && (c as u8).is_ascii_alphabetic() {
                        let txt = format!("\\{}", char::from_u32(c).unwrap());
                        return Err(err(&s, format!("bad escape {txt}"), 2));
                    }
                    literal.push('\\' as u32);
                    literal.push(c);
                }
            }
        }
    }
    if !literal.is_empty() {
        result.push(TemplateItem::Literal(literal));
    }
    Ok(result)
}

/// Expand a parsed template against a match.  `group(i)` must return the
/// matched slice of group `i` (or `None` when the group did not
/// participate, which expands to the empty string).
pub fn expand_template<'a>(
    items: &[TemplateItem],
    mut group: impl FnMut(usize) -> Option<&'a [u32]>,
    out: &mut Vec<u32>,
) {
    for it in items {
        match it {
            TemplateItem::Literal(l) => out.extend_from_slice(l),
            TemplateItem::Group(g) => {
                if let Some(s) = group(*g) {
                    out.extend_from_slice(s);
                }
            }
        }
    }
}
