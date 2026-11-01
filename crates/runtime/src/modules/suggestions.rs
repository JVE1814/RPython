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
