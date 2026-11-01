//! "Did you mean ...?" suggestions (Levenshtein, as CPython's traceback does).
use crate::object::PyRef;
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).map(|j| j * 2).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i * 2; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] {
                0
            } else if a[i - 1].to_lowercase().eq(b[j - 1].to_lowercase()) {
                1
            } else {
                2
            };
            cur[j] = (prev[j] + 2).min(cur[j - 1] + 2).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}
/// `_suggestions._generate_suggestions(candidates, name)`: closest candidate or None.
fn generate_suggestions(_m: &crate::ffi::PyObject, args: &[&crate::ffi::PyObject]) -> crate::errors::PyResult {
    crate::args::check_pos("_generate_suggestions", args, 2, 2)?;
    let items = crate::abstract_::collect_iterable(args[0])?;
    if !crate::types::str::is_str(args[1]) {
        crate::raise!(TypeError, "_generate_suggestions() argument 2 must be str, not {}", crate::types::typeobject::tname(args[1]));
    }
    let strs: Vec<PyRef> = items.into_iter().filter(|i| crate::types::str::is_str(i)).collect();
    let name = crate::types::str::to_string_lossy(args[1]);
    Ok(match suggest_keyword(&name, &strs) {
        Some(s) => crate::types::str::new_str(&s),
        None => crate::types::none::none_ref(),
    })
}

pub fn init_module() -> crate::errors::PyResult {
    let m = crate::types::module::new_module("_suggestions")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("_generate_suggestions", FAST, generate_suggestions)], Some(&m))?;
    Ok(m)
}

pub fn suggest_keyword(name: &str, candidates: &[PyRef]) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for c in candidates {
        let s = crate::types::str::to_string_lossy(c);
        let d = levenshtein(name, &s);
        let max = (name.len() + s.len() + 3) * 2 / 6;
        if d <= max && d > 0 && best.as_ref().map_or(true, |(bd, _)| d < *bd) {
            best = Some((d, s));
        }
    }
    best.map(|(_, s)| s)
}
