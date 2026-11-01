// Docstring/signature editing helper for the docsig workstream (bash/Rust
// tooling only, see AGENTS.md).  Build:
//   rustc -O -o /tmp/docsig_subst tools/docsig_subst.rs
// Usage: docsig_subst RULES
//
// RULES is a text file with these records:
//   @@ <path>                 select the file the following records edit
//   <<< <text>                literal search text ...
//   >>> <text>                ... and its replacement (all occurrences)
//   = name[@func]|SIG|DOC     give the method-table entry ("name", KIND, func ...)
//                             the docstring "name" + SIG + "\n--\n\n" + DOC.
//                             SIG is the text signature, e.g. "($module, /, path)".
//                             DOC "=" keeps the entry's existing doc text (minus any
//                             old signature line).  "@func" picks one entry when
//                             several share the name.  An empty SIG writes DOC only.
// Texts are copied verbatim, so write "\n" for newlines inside Rust strings.
use std::collections::BTreeMap;
use std::fs;

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] as char).is_whitespace() {
        i += 1;
    }
    i
}

/// Parse a Rust string literal starting at b[i] == '"'; return (end index after quote, raw contents).
fn parse_str(s: &str, i: usize) -> (usize, String) {
    let b = s.as_bytes();
    assert_eq!(b[i], b'"');
    let mut j = i + 1;
    while j < b.len() && b[j] != b'"' {
        if b[j] == b'\\' {
            j += 1;
        }
        j += 1;
    }
    (j + 1, s[i + 1..j].to_string())
}

struct Entry {
    start: usize,
    end: usize,
    kind: String,
    func: String,
    doc: Option<String>,
    extra: Option<String>,
}

/// Find entries `("name", KIND, func [, "doc"] [; extra])` in s.
fn find_entries(s: &str, name: &str) -> Vec<Entry> {
    let pat = format!("(\"{name}\",");
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = s[from..].find(&pat) {
        let start = from + p;
        from = start + pat.len();
        let mut i = skip_ws(b, from);
        let ks = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        let kind = s[ks..i].to_string();
        if !matches!(kind.as_str(), "O" | "NOARGS" | "FAST" | "FASTKW") {
            continue;
        }
        i = skip_ws(b, i);
        if i >= b.len() || b[i] != b',' {
            continue;
        }
        i = skip_ws(b, i + 1);
        let fs_ = i;
        let mut depth = 0;
        while i < b.len() {
            match b[i] {
                b'(' | b'[' | b'<' => depth += 1,
                b')' | b']' | b'>' if depth > 0 => depth -= 1,
                b',' | b';' | b')' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let func = s[fs_..i].trim().to_string();
        let mut doc = None;
        if b[i] == b',' {
            let j = skip_ws(b, i + 1);
            if j < b.len() && b[j] == b'"' {
                let (e, d) = parse_str(s, j);
                doc = Some(d);
                i = skip_ws(b, e);
            } else if j < b.len() && b[j] == b')' {
                i = j;
            }
        }
        let mut extra = None;
        if b[i] == b';' {
            let es = i + 1;
            let mut depth = 0;
            while i < b.len() {
                match b[i] {
                    b'(' => depth += 1,
                    b')' if depth == 0 => break,
                    b')' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            extra = Some(s[es..i].trim().to_string());
        }
        if b[i] != b')' {
            continue;
        }
        out.push(Entry { start, end: i + 1, kind, func, doc, extra });
    }
    out
}

/// Escape bare double quotes for use inside a Rust string literal.
fn escape_quotes(s: &str) -> String {
    let mut out = String::new();
    let mut prev = ' ';
    for c in s.chars() {
        if c == '"' && prev != '\\' {
            out.push('\\');
        }
        out.push(c);
        prev = c;
    }
    out
}

fn strip_old_sig(doc: &str) -> String {
    if let Some(p) = doc.find(")\\n--\\n\\n") {
        if !doc[..p].contains("\\n") {
            return doc[p + 8..].to_string();
        }
    }
    doc.to_string()
}

fn main() {
    let rules = std::env::args().nth(1).expect("usage: docsig_subst RULES");
    let text = fs::read_to_string(&rules).expect("read rules");
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut path = String::new();
    let mut old: Option<String> = None;
    let mut failed = 0;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("@@ ") {
            path = p.trim().to_string();
            continue;
        }
        if line.starts_with("<<< ") || line.starts_with(">>> ") || line.starts_with("= ") {
            files
                .entry(path.clone())
                .or_insert_with(|| fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}")));
        }
        if let Some(o) = line.strip_prefix("<<< ") {
            old = Some(o.to_string());
        } else if let Some(n) = line.strip_prefix(">>> ") {
            let o = old.take().expect(">>> without <<<");
            let content = files.get_mut(&path).unwrap();
            if !content.contains(&o) {
                eprintln!("NOT FOUND in {path}: {o}");
                failed += 1;
                continue;
            }
            *content = content.replace(&o, n);
        } else if let Some(r) = line.strip_prefix("= ") {
            let mut parts = r.splitn(3, '|');
            let key = parts.next().unwrap().trim();
            let sig = parts.next().unwrap_or("").trim();
            let doc = parts.next().unwrap_or("");
            let (name, func) = match key.split_once('@') {
                Some((n, f)) => (n, Some(f)),
                None => (key, None),
            };
            let content = files.get_mut(&path).unwrap();
            let mut ents = find_entries(content, name);
            if let Some(f) = func {
                ents.retain(|e| e.func == f);
            }
            if ents.len() != 1 {
                eprintln!("{} entries for {name} in {path}", ents.len());
                failed += 1;
                continue;
            }
            let e = &ents[0];
            let body = if doc == "=" { strip_old_sig(e.doc.as_deref().unwrap_or("")) } else { doc.to_string() };
            let body = if doc == "=" { body } else { escape_quotes(&body) };
            let sig = escape_quotes(sig);
            let full = if sig.is_empty() { body } else { format!("{name}{sig}\\n--\\n\\n{body}") };
            let mut new = format!("(\"{name}\", {}, {}, \"{full}\"", e.kind, e.func);
            if let Some(x) = &e.extra {
                new.push_str("; ");
                new.push_str(x);
            }
            new.push(')');
            content.replace_range(e.start..e.end, &new);
        }
    }
    for (p, c) in &files {
        fs::write(p, c).expect("write");
    }
    eprintln!("{} files written, {} rules failed", files.len(), failed);
    if failed > 0 {
        std::process::exit(1);
    }
}
