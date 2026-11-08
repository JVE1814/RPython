//! Table generator for the `rpy-unicode` crate.
//!
//! Reads the Unicode Character Database files stored under
//! `crates/unicode/data/<version>/` and writes compact static lookup tables
//! to `crates/unicode/src/tables/*.rs`.  Run manually:
//!
//! ```text
//! CARGO_TARGET_DIR=target/unicode cargo run --release \
//!     --manifest-path crates/unicode/gen/Cargo.toml
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const N: usize = 0x110000;
const VERSION: &str = "17.0.0";

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s.trim(), 16).unwrap_or_else(|_| panic!("bad hex {s:?}"))
}

fn parse_range(s: &str) -> (u32, u32) {
    let s = s.trim();
    if let Some((a, b)) = s.split_once("..") {
        (hex(a), hex(b))
    } else {
        let v = hex(s);
        (v, v)
    }
}

/// Data lines of a UCD property file: `range ; field ; field # comment`.
fn data_lines(text: &str) -> Vec<(u32, u32, Vec<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(';').map(|s| s.trim().to_string());
        let (a, b) = parse_range(&parts.next().unwrap());
        out.push((a, b, parts.collect()));
    }
    out
}

/// `# @missing: range; field; field` lines.
fn missing_lines(text: &str) -> Vec<(u32, u32, Vec<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# @missing:") {
            let mut parts = rest.split(';').map(|s| s.trim().to_string());
            let (a, b) = parse_range(&parts.next().unwrap());
            out.push((a, b, parts.collect()));
        }
    }
    out
}

fn cps(s: &str) -> Vec<u32> {
    s.split_whitespace().map(hex).collect()
}

// ---------------------------------------------------------------------------
// UnicodeData.txt
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Rec {
    name: String,
    cat: String,
    ccc: u8,
    bidi: String,
    decomp: String,
    decimal: String,
    digit: String,
    numeric: String,
    mirrored: bool,
    upper: Option<u32>,
    lower: Option<u32>,
    title: Option<u32>,
}

struct Ucd {
    idx: Vec<u32>,
    recs: Vec<Rec>,
}

impl Ucd {
    fn get(&self, cp: u32) -> Option<&Rec> {
        let i = self.idx[cp as usize];
        if i == u32::MAX {
            None
        } else {
            Some(&self.recs[i as usize])
        }
    }
}

fn load_unicode_data(path: &Path) -> Ucd {
    let text = read(path);
    let mut idx = vec![u32::MAX; N];
    let mut recs: Vec<Rec> = Vec::new();
    let mut range_start: Option<u32> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(';').collect();
        assert!(f.len() >= 15, "{line}");
        let cp = hex(f[0]);
        let opt = |s: &str| if s.trim().is_empty() { None } else { Some(hex(s)) };
        let rec = Rec {
            name: f[1].to_string(),
            cat: f[2].to_string(),
            ccc: f[3].parse().unwrap(),
            bidi: f[4].to_string(),
            decomp: f[5].to_string(),
            decimal: f[6].to_string(),
            digit: f[7].to_string(),
            numeric: f[8].to_string(),
            mirrored: f[9] == "Y",
            upper: opt(f[12]),
            lower: opt(f[13]),
            title: opt(f[14]),
        };
        if rec.name.ends_with(", First>") {
            range_start = Some(cp);
            continue;
        }
        if rec.name.ends_with(", Last>") {
            let start = range_start.take().expect("Last without First");
            let mut r = rec.clone();
            r.name = String::new();
            recs.push(r);
            for c in start..=cp {
                idx[c as usize] = (recs.len() - 1) as u32;
            }
            continue;
        }
        recs.push(rec);
        idx[cp as usize] = (recs.len() - 1) as u32;
    }
    Ucd { idx, recs }
}

/// Patch numeric values from Unihan (kAccountingNumeric, kOtherNumeric,
/// kPrimaryNumeric), only for assigned code points.
fn patch_unihan(ucd: &mut Ucd, path: &Path) {
    let text = read(path);
    for line in text.lines() {
        if !line.starts_with("U+") {
            continue;
        }
        let mut it = line.split_whitespace();
        let code = it.next().unwrap();
        let tag = it.next().unwrap();
        let value = it.next().unwrap();
        if !matches!(tag, "kAccountingNumeric" | "kOtherNumeric" | "kPrimaryNumeric") {
            continue;
        }
        let cp = hex(&code[2..]);
        let value = value.replace(',', "");
        // Records for range members are shared: copy-on-write.
        if ucd.get(cp).is_some() {
            let mut r = ucd.get(cp).unwrap().clone();
            r.numeric = value;
            ucd.recs.push(r);
            ucd.idx[cp as usize] = (ucd.recs.len() - 1) as u32;
        }
    }
}

fn parse_numeric(s: &str) -> f64 {
    if let Some((a, b)) = s.split_once('/') {
        a.parse::<f64>().unwrap() / b.parse::<f64>().unwrap()
    } else {
        s.parse::<f64>().unwrap()
    }
}

// ---------------------------------------------------------------------------
// Multi-level trie
// ---------------------------------------------------------------------------

struct Trie {
    s1: u32,
    s2: u32,
    i1: Vec<u32>,
    i2: Vec<u32>,
    leaf: Vec<u32>,
}

fn split(data: &[u32], shift: u32) -> (Vec<u32>, Vec<u32>) {
    let size = 1usize << shift;
    assert_eq!(data.len() % size, 0);
    let mut map: HashMap<&[u32], u32> = HashMap::new();
    let mut index = Vec::with_capacity(data.len() / size);
    let mut blocks = Vec::new();
    for chunk in data.chunks(size) {
        let id = *map.entry(chunk).or_insert_with(|| {
            let id = (blocks.len() / size) as u32;
            blocks.extend_from_slice(chunk);
            id
        });
        index.push(id);
    }
    (index, blocks)
}

fn elem_bytes(max: u32) -> usize {
    if max <= 0xFF {
        1
    } else if max <= 0xFFFF {
        2
    } else {
        4
    }
}

fn arr_bytes(v: &[u32]) -> usize {
    v.len() * elem_bytes(v.iter().copied().max().unwrap_or(0))
}

fn build_trie(values: &[u32]) -> Trie {
    assert_eq!(values.len(), N);
    let mut best: Option<(usize, Trie)> = None;
    for s2 in 2..=9 {
        let (idx2, leaf) = split(values, s2);
        for s1 in 2..=8 {
            if s1 + s2 > 16 {
                continue;
            }
            let (i1, i2) = split(&idx2, s1);
            let size = arr_bytes(&i1) + arr_bytes(&i2) + arr_bytes(&leaf);
            if best.as_ref().map_or(true, |(b, _)| size < *b) {
                best = Some((size, Trie { s1, s2, i1, i2: i2.clone(), leaf: leaf.clone() }));
            }
        }
    }
    let t = best.unwrap().1;
    // verify
    for cp in 0..N {
        let a = t.i1[cp >> (t.s1 + t.s2)] as usize;
        let b = t.i2[(a << t.s1) | ((cp >> t.s2) & ((1 << t.s1) - 1))] as usize;
        let v = t.leaf[(b << t.s2) | (cp & ((1 << t.s2) - 1))];
        assert_eq!(v, values[cp]);
    }
    t
}

fn ty_for(max: u32) -> &'static str {
    match elem_bytes(max) {
        1 => "u8",
        2 => "u16",
        _ => "u32",
    }
}

struct Out {
    s: String,
    bytes: usize,
    /// (mem1) Arrays written while set go to the `.rodata.rpycold` section, which the
    /// launcher's link places after the other read-only data: tables a normal start never
    /// reads stay out of the fault-around windows of the constants it does read.
    cold: bool,
}

impl Out {
    fn new(header: &str) -> Out {
        let mut s = String::new();
        s.push_str("// @generated by crates/unicode/gen; do not edit.\n");
        s.push_str(&format!("// Unicode {VERSION} (current) and 3.2.0 data.\n"));
        s.push_str(header);
        Out { s, bytes: 0, cold: false }
    }

    fn array(&mut self, name: &str, ty: &str, vals: impl IntoIterator<Item = String>, elem_size: usize) {
        let vals: Vec<String> = vals.into_iter().collect();
        self.bytes += vals.len() * elem_size;
        if self.cold {
            self.s.push_str("#[unsafe(link_section = \".rodata.rpycold\")]\n");
        }
        writeln!(self.s, "pub(crate) static {name}: [{ty}; {}] = [", vals.len()).unwrap();
        let mut line = String::new();
        for v in vals {
            if line.len() + v.len() > 100 {
                self.s.push_str(&line);
                self.s.push('\n');
                line.clear();
            }
            line.push_str(&v);
            line.push(',');
        }
        if !line.is_empty() {
            self.s.push_str(&line);
            self.s.push('\n');
        }
        self.s.push_str("];\n");
    }

    fn num_array(&mut self, name: &str, vals: &[u32]) -> &'static str {
        let max = vals.iter().copied().max().unwrap_or(0);
        let ty = ty_for(max);
        self.array(name, ty, vals.iter().map(|v| v.to_string()), elem_bytes(max));
        ty
    }

    fn trie(&mut self, name: &str, values: &[u32]) {
        let t = build_trie(values);
        let up = name.to_uppercase();
        self.num_array(&format!("{up}_I1"), &t.i1);
        self.num_array(&format!("{up}_I2"), &t.i2);
        let ty = self.num_array(&format!("{up}_LEAF"), &t.leaf);
        writeln!(
            self.s,
            "#[inline]\npub(crate) fn {name}(cp: u32) -> {ty} {{\n    \
             let cp = if cp > 0x10FFFF {{ 0x10FFFF }} else {{ cp }} as usize;\n    \
             let a = {up}_I1[cp >> {}] as usize;\n    \
             let b = {up}_I2[(a << {}) | ((cp >> {}) & {})] as usize;\n    \
             {up}_LEAF[(b << {}) | (cp & {})]\n}}",
            t.s1 + t.s2,
            t.s1,
            t.s2,
            (1 << t.s1) - 1,
            t.s2,
            (1 << t.s2) - 1
        )
        .unwrap();
    }

    fn write(&self, path: &Path) {
        fs::write(path, &self.s).unwrap();
        eprintln!("wrote {} ({} bytes of table data)", path.display(), self.bytes);
    }
}

fn rust_str(s: &str) -> String {
    format!("{s:?}")
}

// ---------------------------------------------------------------------------
// Constants shared with the runtime crate
// ---------------------------------------------------------------------------

const CATEGORIES: &[&str] = &[
    "Cn", "Lu", "Ll", "Lt", "Mn", "Mc", "Me", "Nd", "Nl", "No", "Zs", "Zl", "Zp", "Cc", "Cf",
    "Cs", "Co", "Cn", "Lm", "Lo", "Pc", "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Sm", "Sc", "Sk",
    "So",
];
const BIDI: &[&str] = &[
    "", "L", "LRE", "LRO", "R", "AL", "RLE", "RLO", "PDF", "EN", "ES", "ET", "AN", "CS", "NSM",
    "BN", "B", "S", "WS", "ON", "LRI", "RLI", "FSI", "PDI",
];
const EAW: &[&str] = &["F", "H", "W", "Na", "A", "N"];
const GCB: &[&str] = &[
    "Other", "Prepend", "CR", "LF", "Control", "Extend", "Regional_Indicator", "SpacingMark", "L",
    "V", "T", "LV", "LVT", "ZWJ",
];
const INCB: &[&str] = &["None", "Linker", "Consonant", "Extend"];
const DECOMP_TAGS: &[&str] = &[
    "", "<font>", "<noBreak>", "<initial>", "<medial>", "<final>", "<isolated>", "<circle>",
    "<super>", "<sub>", "<vertical>", "<wide>", "<narrow>", "<small>", "<square>", "<fraction>",
    "<compat>",
];

fn cat_index(c: &str) -> u32 {
    // index 0 and 17 are both "Cn"; use 0
    CATEGORIES.iter().position(|x| *x == c).unwrap_or_else(|| panic!("cat {c}")) as u32
}
fn bidi_index(c: &str) -> u32 {
    BIDI.iter().position(|x| *x == c).unwrap_or_else(|| panic!("bidi {c}")) as u32
}
fn eaw_index(c: &str) -> u32 {
    EAW.iter().position(|x| *x == c).unwrap_or_else(|| panic!("eaw {c}")) as u32
}

fn bidi_long(s: &str) -> &str {
    match s {
        "Left_To_Right" => "L",
        "Right_To_Left" => "R",
        "Arabic_Letter" => "AL",
        "European_Terminator" => "ET",
        "Boundary_Neutral" => "BN",
        other => other,
    }
}

// ctype flags (must match src/lib.rs)
const ALPHA: u32 = 1 << 0;
const DECIMAL: u32 = 1 << 1;
const DIGIT: u32 = 1 << 2;
const NUMERIC: u32 = 1 << 3;
const LOWER: u32 = 1 << 4;
const UPPER: u32 = 1 << 5;
const TITLE: u32 = 1 << 6;
const CASED: u32 = 1 << 7;
const CASE_IGNORABLE: u32 = 1 << 8;
const SPACE: u32 = 1 << 9;
const LINEBREAK: u32 = 1 << 10;
const PRINTABLE: u32 = 1 << 11;
const XID_START: u32 = 1 << 12;
const XID_CONTINUE: u32 = 1 << 13;
const EXTENDED_CASE: u32 = 1 << 14;

// db flags (must match src/lib.rs)
const MIRRORED: u32 = 1 << 0;
const NFD_NO: u32 = 1 << 1;
const NFKD_NO: u32 = 1 << 2;
const NFC_NO: u32 = 1 << 3;
const NFC_MAYBE: u32 = 1 << 4;
const NFKC_NO: u32 = 1 << 5;
const NFKC_MAYBE: u32 = 1 << 6;
const EXT_PICT: u32 = 1 << 7;

fn binary_props(text: &str) -> HashMap<String, Vec<bool>> {
    let mut m: HashMap<String, Vec<bool>> = HashMap::new();
    for (a, b, f) in data_lines(text) {
        if f.len() != 1 {
            continue;
        }
        let v = m.entry(f[0].clone()).or_insert_with(|| vec![false; N]);
        for c in a..=b {
            v[c as usize] = true;
        }
    }
    m
}

/// Full-resolution decomposition (decomposition() string).
fn decomp_parts(s: &str) -> (u32, Vec<u32>) {
    let s = s.trim();
    if s.is_empty() {
        return (0, vec![]);
    }
    if s.starts_with('<') {
        let end = s.find('>').unwrap();
        let tag = &s[..=end];
        let t = DECOMP_TAGS.iter().position(|x| *x == tag).unwrap_or_else(|| panic!("tag {tag}"));
        (t as u32, cps(&s[end + 1..]))
    } else {
        (0, cps(s))
    }
}

// ---------------------------------------------------------------------------

fn main() {
    let root: PathBuf = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    });
    let d = root.join("data").join(VERSION);
    let d32 = root.join("data").join("3.2.0");
    let outdir = root.join("src").join("tables");
    fs::create_dir_all(&outdir).unwrap();

    let mut ucd = load_unicode_data(&d.join("UnicodeData.txt"));
    patch_unihan(&mut ucd, &d.join("Unihan_NumericValues.txt"));
    let mut ucd32 = load_unicode_data(&d32.join("UnicodeData-3.2.0.txt"));
    patch_unihan(&mut ucd32, &d32.join("Unihan-3.2.0-NumericValues.txt"));

    let core = binary_props(&read(&d.join("DerivedCoreProperties.txt")));
    let normprops_text = read(&d.join("DerivedNormalizationProps.txt"));
    let normprops = binary_props(&normprops_text);

    // ---- special casing / case folding
    let mut special: HashMap<u32, (Vec<u32>, Vec<u32>, Vec<u32>)> = HashMap::new();
    for line in read(&d.join("SpecialCasing.txt")).lines() {
        let line = line.split('#').next().unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(';').map(|s| s.trim()).collect();
        if f.len() > 4 && !f[4].is_empty() {
            continue; // conditional mapping
        }
        special.insert(hex(f[0]), (cps(f[1]), cps(f[2]), cps(f[3])));
    }
    let mut folding: HashMap<u32, Vec<u32>> = HashMap::new();
    for (a, _, f) in data_lines(&read(&d.join("CaseFolding.txt"))) {
        if f[0] == "C" || f[0] == "F" {
            folding.insert(a, cps(&f[1]));
        }
    }

    // ---- bidi defaults
    let bidi_text = read(&d.join("DerivedBidiClass.txt"));
    let mut bidi_all = vec![String::new(); N];
    for (a, b, f) in missing_lines(&bidi_text) {
        for c in a..=b {
            bidi_all[c as usize] = bidi_long(&f[0]).to_string();
        }
    }
    for (a, b, f) in data_lines(&bidi_text) {
        for c in a..=b {
            bidi_all[c as usize] = f[0].clone();
        }
    }
    // ---- east asian width
    let eaw_text = read(&d.join("EastAsianWidth.txt"));
    let mut eaw_all = vec!["N".to_string(); N];
    for (a, b, f) in missing_lines(&eaw_text).into_iter().chain(data_lines(&eaw_text)) {
        for c in a..=b {
            eaw_all[c as usize] = f[0].clone();
        }
    }
    // ---- grapheme break, incb, extpict
    let mut gcb_all = vec![0u32; N];
    for (a, b, f) in data_lines(&read(&d.join("GraphemeBreakProperty.txt"))) {
        let v = GCB.iter().position(|x| *x == f[0]).unwrap_or_else(|| panic!("gcb {}", f[0]));
        for c in a..=b {
            gcb_all[c as usize] = v as u32;
        }
    }
    let mut incb_all = vec![0u32; N];
    for (a, b, f) in data_lines(&read(&d.join("DerivedCoreProperties.txt"))) {
        if f.len() == 2 && f[0] == "InCB" {
            let v = INCB.iter().position(|x| *x == f[1]).unwrap();
            for c in a..=b {
                incb_all[c as usize] = v as u32;
            }
        }
    }
    let mut extpict = vec![false; N];
    for (a, b, f) in data_lines(&read(&d.join("emoji-data.txt"))) {
        if f[0] == "Extended_Pictographic" {
            for c in a..=b {
                extpict[c as usize] = true;
            }
        }
    }
    // ---- normalization quick check
    let mut qc = vec![0u32; N];
    for (a, b, f) in data_lines(&normprops_text) {
        if f.len() != 2 {
            continue;
        }
        let bit = match (f[0].as_str(), f[1].as_str()) {
            ("NFD_QC", "N") => NFD_NO,
            ("NFKD_QC", "N") => NFKD_NO,
            ("NFC_QC", "N") => NFC_NO,
            ("NFC_QC", "M") => NFC_MAYBE,
            ("NFKC_QC", "N") => NFKC_NO,
            ("NFKC_QC", "M") => NFKC_MAYBE,
            _ => continue,
        };
        for c in a..=b {
            qc[c as usize] |= bit;
        }
    }

    // =======================================================================
    // ctype records
    // =======================================================================
    let mut numeric_values: Vec<f64> = Vec::new();
    let mut numeric_map: HashMap<u64, u32> = HashMap::new();
    let mut numeric_index = |v: f64| -> u32 {
        *numeric_map.entry(v.to_bits()).or_insert_with(|| {
            numeric_values.push(v);
            (numeric_values.len() - 1) as u32
        })
    };
    let mut special_data: Vec<u32> = vec![0]; // offset 0 unused
    let mut special_map: HashMap<Vec<u32>, u32> = HashMap::new();
    type CRec = (u32, i32, i32, i32, u32, u32, u32, u32);
    let mut crecs: Vec<CRec> = Vec::new();
    let mut crec_map: HashMap<CRec, u32> = HashMap::new();
    let mut ctype_idx = vec![0u32; N];
    let mut fold_no_entry_with_lower = 0;
    for cp in 0..N as u32 {
        let r = ucd.get(cp);
        let cat = r.map(|r| r.cat.as_str()).unwrap_or("Cn");
        let bidi = r.map(|r| r.bidi.as_str()).unwrap_or("");
        let c = cp as usize;
        let mut flags = 0;
        if matches!(cat, "Ll" | "Lu" | "Lt" | "Lm" | "Lo") {
            flags |= ALPHA;
        }
        let mut decimal = 0xFF;
        let mut digit = 0xFF;
        let mut numeric = 0xFFFF;
        if let Some(r) = r {
            if !r.decimal.is_empty() {
                flags |= DECIMAL;
                decimal = r.decimal.parse().unwrap();
            }
            if !r.digit.is_empty() {
                flags |= DIGIT;
                digit = r.digit.parse().unwrap();
            }
            if !r.numeric.is_empty() {
                flags |= NUMERIC;
                numeric = numeric_index(parse_numeric(&r.numeric));
            }
        }
        let has = |p: &str| core.get(p).map_or(false, |v| v[c]);
        if has("Lowercase") {
            flags |= LOWER;
        }
        if has("Uppercase") {
            flags |= UPPER;
        }
        if cat == "Lt" {
            flags |= TITLE;
        }
        if has("Cased") {
            flags |= CASED;
        }
        if has("Case_Ignorable") {
            flags |= CASE_IGNORABLE;
        }
        if cat == "Zs" || matches!(bidi, "WS" | "B" | "S") {
            flags |= SPACE;
        }
        if bidi == "B" || matches!(cp, 0x0A | 0x0B | 0x0C | 0x0D | 0x85 | 0x2028 | 0x2029) {
            flags |= LINEBREAK;
        }
        if cp == 0x20 || !(cat.starts_with('C') || cat.starts_with('Z')) {
            flags |= PRINTABLE;
        }
        if has("XID_Start") {
            flags |= XID_START;
        }
        if has("XID_Continue") {
            flags |= XID_CONTINUE;
        }
        let upper = r.and_then(|r| r.upper).unwrap_or(cp);
        let lower = r.and_then(|r| r.lower).unwrap_or(cp);
        let title = r.and_then(|r| r.title).unwrap_or(upper);
        let mut sc = special.get(&cp).cloned();
        let cf = folding.get(&cp).cloned().unwrap_or_else(|| vec![cp]);
        if !folding.contains_key(&cp) && lower != cp {
            fold_no_entry_with_lower += 1;
        }
        if sc.is_none() && cf != vec![lower] {
            sc = Some((vec![lower], vec![title], vec![upper]));
        }
        let mut sp = 0;
        if let Some((l, t, u)) = sc {
            flags |= EXTENDED_CASE;
            assert!(l.len() <= 3 && t.len() <= 3 && u.len() <= 3 && cf.len() <= 3);
            let mut entry = vec![(l.len() | t.len() << 4 | u.len() << 8 | cf.len() << 12) as u32];
            entry.extend(&l);
            entry.extend(&t);
            entry.extend(&u);
            entry.extend(&cf);
            sp = *special_map.entry(entry.clone()).or_insert_with(|| {
                let off = special_data.len() as u32;
                special_data.extend(&entry);
                off
            });
        }
        let rec: CRec = (
            flags,
            upper as i32 - cp as i32,
            lower as i32 - cp as i32,
            title as i32 - cp as i32,
            decimal,
            digit,
            numeric,
            sp,
        );
        let id = *crec_map.entry(rec).or_insert_with(|| {
            crecs.push(rec);
            (crecs.len() - 1) as u32
        });
        ctype_idx[c] = id;
    }
    eprintln!("ctype records: {}, special entries: {} words, numeric values: {}, fold-without-entry: {}",
        crecs.len(), special_data.len(), numeric_values.len(), fold_no_entry_with_lower);

    // =======================================================================
    // db records
    // =======================================================================
    type DRec = (u32, u32, u32, u32, u32, u32, u32);
    let mut drecs: Vec<DRec> = Vec::new();
    let mut drec_map: HashMap<DRec, u32> = HashMap::new();
    let mut db_idx = vec![0u32; N];
    for cp in 0..N as u32 {
        let c = cp as usize;
        let r = ucd.get(cp);
        let cat = cat_index(r.map(|r| r.cat.as_str()).unwrap_or("Cn"));
        let ccc = r.map(|r| r.ccc as u32).unwrap_or(0);
        let bidi = match r {
            Some(r) => {
                assert_eq!(r.bidi, bidi_all[c], "bidi mismatch {cp:X}");
                bidi_index(&r.bidi)
            }
            None => bidi_index(&bidi_all[c]),
        };
        let eaw = eaw_index(&eaw_all[c]);
        let mut flags = qc[c];
        if r.map_or(false, |r| r.mirrored) {
            flags |= MIRRORED;
        }
        if extpict[c] {
            flags |= EXT_PICT;
        }
        let rec: DRec = (cat, ccc, bidi, eaw, flags, gcb_all[c], incb_all[c]);
        let id = *drec_map.entry(rec).or_insert_with(|| {
            drecs.push(rec);
            (drecs.len() - 1) as u32
        });
        db_idx[c] = id;
    }
    eprintln!("db records: {}", drecs.len());

    // =======================================================================
    // decompositions
    // =======================================================================
    let mut decomp_data: Vec<u32> = vec![0];
    let mut decomp_idx = vec![0u32; N];
    let mut decomp_full: HashMap<u32, (u32, Vec<u32>)> = HashMap::new();
    for cp in 0..N as u32 {
        if let Some(r) = ucd.get(cp) {
            let (tag, v) = decomp_parts(&r.decomp);
            if v.is_empty() {
                continue;
            }
            decomp_idx[cp as usize] = decomp_data.len() as u32;
            decomp_data.push(tag << 8 | v.len() as u32);
            decomp_data.extend(&v);
            decomp_full.insert(cp, (tag, v));
        }
    }
    // composition pairs
    let excl = &normprops["Full_Composition_Exclusion"];
    let mut comp: Vec<(u32, u32, u32)> = Vec::new();
    for (&cp, (tag, v)) in &decomp_full {
        if *tag == 0 && v.len() == 2 && !excl[cp as usize] {
            comp.push((v[0], v[1], cp));
        }
    }
    comp.sort();
    eprintln!("decomp words {}, comp pairs {}", decomp_data.len(), comp.len());

    // =======================================================================
    // emit tables/db.rs
    // =======================================================================
    let mut o = Out::new("#![allow(clippy::all)]\nuse super::super::{TypeRecord, DbRecord};\n\n");
    writeln!(o.s, "pub const UNIDATA_VERSION: &str = {};", rust_str(VERSION)).unwrap();
    o.trie("ctype_index", &ctype_idx);
    o.bytes += crecs.len() * 24;
    writeln!(o.s, "pub(crate) static TYPE_RECORDS: [TypeRecord; {}] = [", crecs.len()).unwrap();
    for r in &crecs {
        writeln!(
            o.s,
            "TypeRecord{{flags:{},upper:{},lower:{},title:{},decimal:{},digit:{},numeric:{},special:{}}},",
            r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7
        )
        .unwrap();
    }
    o.s.push_str("];\n");
    o.num_array("SPECIAL_CASE", &special_data);
    o.bytes += numeric_values.len() * 8;
    writeln!(o.s, "pub(crate) static NUMERIC_VALUES: [f64; {}] = [", numeric_values.len()).unwrap();
    for v in &numeric_values {
        writeln!(o.s, "{v:?},").unwrap();
    }
    o.s.push_str("];\n");

    o.trie("db_index", &db_idx);
    o.bytes += drecs.len() * 7;
    writeln!(o.s, "pub(crate) static DB_RECORDS: [DbRecord; {}] = [", drecs.len()).unwrap();
    for r in &drecs {
        writeln!(
            o.s,
            "DbRecord{{category:{},combining:{},bidi:{},eaw:{},flags:{},gcb:{},incb:{}}},",
            r.0, r.1, r.2, r.3, r.4, r.5, r.6
        )
        .unwrap();
    }
    o.s.push_str("];\n");
    o.cold = true; // normalization only
    o.trie("decomp_index", &decomp_idx);
    o.num_array("DECOMP_DATA", &decomp_data);
    o.cold = false;
    o.bytes += comp.len() * 12;
    o.s.push_str("#[unsafe(link_section = \".rodata.rpycold\")]\n"); // normalization only
    writeln!(o.s, "pub(crate) static COMP_PAIRS: [(u32, u32, u32); {}] = [", comp.len()).unwrap();
    for (a, b, c) in &comp {
        write!(o.s, "(0x{a:X},0x{b:X},0x{c:X}),").unwrap();
    }
    o.s.push_str("\n];\n");

    let str_list = |o: &mut Out, name: &str, l: &[&str]| {
        writeln!(o.s, "pub(crate) static {name}: [&str; {}] = [{}];", l.len(),
            l.iter().map(|s| rust_str(s)).collect::<Vec<_>>().join(", ")).unwrap();
    };
    str_list(&mut o, "CATEGORY_NAMES", CATEGORIES);
    str_list(&mut o, "BIDI_NAMES", BIDI);
    str_list(&mut o, "EAW_NAMES", EAW);
    str_list(&mut o, "GCB_NAMES", GCB);
    str_list(&mut o, "INCB_NAMES", INCB);
    str_list(&mut o, "DECOMP_TAGS", DECOMP_TAGS);

    // blocks
    let blocks = data_lines(&read(&d.join("Blocks.txt")));
    writeln!(o.s, "pub(crate) static BLOCKS: [(u32, u32, &str); {}] = [", blocks.len()).unwrap();
    for (a, b, f) in &blocks {
        writeln!(o.s, "(0x{a:X}, 0x{b:X}, {}),", rust_str(&f[0])).unwrap();
    }
    o.s.push_str("];\n");
    o.write(&outdir.join("db.rs"));

    // =======================================================================
    // names
    // =======================================================================
    gen_names(&d, &outdir, &ucd);

    // =======================================================================
    // 3.2.0
    // =======================================================================
    gen_v320(&outdir, &ucd, &ucd32, &eaw_all, &d32);
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

fn gen_names(d: &Path, outdir: &Path, ucd: &Ucd) {
    let text = read(&d.join("DerivedName.txt"));
    let mut explicit: Vec<(String, u32)> = Vec::new();
    let mut algo: Vec<(u32, u32, String)> = Vec::new();
    let mut hangul = 0;
    for (a, b, f) in data_lines(&text) {
        let name = &f[0];
        if let Some(prefix) = name.strip_suffix('*') {
            algo.push((a, b, prefix.to_string()));
        } else {
            assert_eq!(a, b);
            if name.starts_with("HANGUL SYLLABLE ") && (0xAC00..=0xD7A3).contains(&a) {
                hangul += 1;
                continue;
            }
            explicit.push((name.clone(), a));
        }
    }
    assert_eq!(hangul, 11172);
    // sanity: explicit names agree with UnicodeData
    for (n, cp) in &explicit {
        let r = ucd.get(*cp).unwrap();
        assert!(r.name == *n || r.name.starts_with('<'), "{cp:X} {n} {}", r.name);
    }
    explicit.sort();
    assert!(explicit.len() < 0xFFFF);

    // word statistics
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for (n, _) in &explicit {
        for w in n.split(' ') {
            *freq.entry(w).or_default() += 1;
        }
    }
    let mut words: Vec<(&str, usize)> = freq.into_iter().collect();
    words.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let word_id: HashMap<&str, usize> = words.iter().enumerate().map(|(i, (w, _))| (*w, i)).collect();
    const ONE: usize = 0xC0;
    assert!(words.len() <= ONE + 63 * 256, "too many words: {}", words.len());

    let mut data: Vec<u32> = Vec::new();
    let mut blocks: Vec<u32> = Vec::new();
    let mut name_idx = vec![0xFFFFu32; N];
    for (i, (n, cp)) in explicit.iter().enumerate() {
        if i % 16 == 0 {
            blocks.push(data.len() as u32);
        }
        let mut codes = Vec::new();
        for w in n.split(' ') {
            let k = word_id[w];
            if k < ONE {
                codes.push(k as u32);
            } else {
                let k = k - ONE;
                codes.push((ONE + (k >> 8)) as u32);
                codes.push((k & 0xFF) as u32);
            }
        }
        assert!(codes.len() < 256);
        data.push(codes.len() as u32);
        data.push(cp >> 16);
        data.push((cp >> 8) & 0xFF);
        data.push(cp & 0xFF);
        data.extend(codes);
        name_idx[*cp as usize] = i as u32;
    }
    let mut wdata: Vec<u32> = Vec::new();
    let mut wblocks: Vec<u32> = Vec::new();
    for (i, (w, _)) in words.iter().enumerate() {
        if i % 16 == 0 {
            wblocks.push(wdata.len() as u32);
        }
        assert!(w.len() < 256);
        wdata.push(w.len() as u32);
        wdata.extend(w.bytes().map(|b| b as u32));
    }
    let maxlen = explicit.iter().map(|(n, _)| n.len()).max().unwrap();

    let mut o = Out::new("#![allow(clippy::all)]\n\n");
    writeln!(o.s, "pub(crate) const NAME_COUNT: usize = {};", explicit.len()).unwrap();
    writeln!(o.s, "pub(crate) const NAME_MAX_LEN: usize = {maxlen};").unwrap();
    writeln!(o.s, "pub(crate) const WORD_ONE_BYTE: u8 = {ONE};").unwrap();
    o.cold = true; // unicodedata.name/lookup and \N{...} only
    o.trie("name_index", &name_idx);
    o.num_array("NAME_DATA", &data);
    o.num_array("NAME_BLOCKS", &blocks);
    o.num_array("WORD_DATA", &wdata);
    o.num_array("WORD_BLOCKS", &wblocks);

    // algorithmic ranges
    let mut prefixes: Vec<String> = algo.iter().map(|x| x.2.clone()).collect();
    prefixes.sort();
    prefixes.dedup();
    writeln!(o.s, "pub(crate) static ALGO_PREFIXES: [&str; {}] = [{}];", prefixes.len(),
        prefixes.iter().map(|s| rust_str(s)).collect::<Vec<_>>().join(", ")).unwrap();
    writeln!(o.s, "pub(crate) static ALGO_RANGES: [(u32, u32, u8); {}] = [", algo.len()).unwrap();
    for (a, b, p) in &algo {
        let pi = prefixes.iter().position(|x| x == p).unwrap();
        writeln!(o.s, "(0x{a:X}, 0x{b:X}, {pi}),").unwrap();
    }
    o.s.push_str("];\n");

    // aliases
    let mut aliases: Vec<(String, u32)> = Vec::new();
    for (a, _, f) in data_lines(&read(&d.join("NameAliases.txt"))) {
        aliases.push((f[0].clone(), a));
    }
    aliases.sort();
    writeln!(o.s, "pub(crate) static ALIASES: [(&str, u32); {}] = [", aliases.len()).unwrap();
    for (n, cp) in &aliases {
        o.bytes += n.len() + 20;
        writeln!(o.s, "({}, 0x{cp:X}),", rust_str(n)).unwrap();
    }
    o.s.push_str("];\n");
    // named sequences
    let mut seqs: Vec<(String, Vec<u32>)> = Vec::new();
    for line in read(&d.join("NamedSequences.txt")).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (n, c) = line.split_once(';').unwrap();
        seqs.push((n.trim().to_string(), cps(c)));
    }
    seqs.sort();
    writeln!(o.s, "pub(crate) static NAMED_SEQUENCES: [(&str, &[u32]); {}] = [", seqs.len()).unwrap();
    for (n, c) in &seqs {
        o.bytes += n.len() + 20 + 4 * c.len();
        writeln!(o.s, "({}, &[{}]),", rust_str(n),
            c.iter().map(|x| format!("0x{x:X}")).collect::<Vec<_>>().join(",")).unwrap();
    }
    o.s.push_str("];\n");
    eprintln!("names: {} explicit, {} words, {} aliases, {} sequences, max len {maxlen}",
        explicit.len(), words.len(), aliases.len(), seqs.len());
    o.write(&outdir.join("names.rs"));
}

// ---------------------------------------------------------------------------
// Unicode 3.2.0 deltas
// ---------------------------------------------------------------------------

fn gen_v320(
    outdir: &Path,
    ucd: &Ucd,
    ucd32: &Ucd,
    eaw_all: &[String],
    d32: &Path,
) {
    let eaw_text = read(&d32.join("EastAsianWidth-3.2.0.txt"));
    let mut eaw32: Vec<Option<String>> = vec![None; N];
    for (a, b, f) in data_lines(&eaw_text) {
        for c in a..=b {
            eaw32[c as usize] = Some(f[0].clone());
        }
    }
    // assigned ranges in 3.2.0
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for cp in 0..N as u32 {
        if ucd32.get(cp).is_some() {
            match ranges.last_mut() {
                Some(r) if r.1 + 1 == cp => r.1 = cp,
                _ => ranges.push((cp, cp)),
            }
        }
    }
    // change records for chars assigned in 3.2.0
    type Chg = (u32, u32, u32, u32, u32, String);
    let mut recs: Vec<Chg> = Vec::new();
    let mut map: HashMap<Chg, u32> = HashMap::new();
    let mut changes: Vec<(u32, u32)> = Vec::new();
    let mut corrections: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut numeric_strings: BTreeMap<String, f64> = BTreeMap::new();
    let mut report: BTreeMap<String, usize> = BTreeMap::new();
    for cp in 0..N as u32 {
        let Some(old) = ucd32.get(cp) else { continue };
        let new = ucd.get(cp).expect("assigned in 3.2 but not now");
        let c = cp as usize;
        let old_eaw = eaw32[c].clone().unwrap_or_else(|| "N".into());
        let new_eaw = eaw_all[c].clone();
        if old.ccc != new.ccc {
            *report.entry("ccc".into()).or_default() += 1;
        }
        if old.digit != new.digit {
            *report.entry("digit".into()).or_default() += 1;
            eprintln!("digit differs {cp:04X}: {:?} -> {:?}", old.digit, new.digit);
        }
        if old.decomp != new.decomp {
            *report.entry("decomp".into()).or_default() += 1;
            let (ot, ov) = decomp_parts(&old.decomp);
            let (nt, _) = decomp_parts(&new.decomp);
            eprintln!("decomp differs {cp:04X}: {:?} -> {:?}", old.decomp, new.decomp);
            assert!(ot == 0 && nt == 0);
            corrections.push((cp, ov));
        }
        if old.upper != new.upper || old.lower != new.lower || old.title != new.title {
            *report.entry("case".into()).or_default() += 1;
        }
        let differs = old.cat != new.cat
            || old.bidi != new.bidi
            || old.mirrored != new.mirrored
            || old.decimal != new.decimal
            || old.numeric != new.numeric
            || old_eaw != new_eaw;
        if !differs {
            continue;
        }
        if !old.numeric.is_empty() {
            numeric_strings.insert(old.numeric.clone(), parse_numeric(&old.numeric));
        }
        let rec: Chg = (
            cat_index(&old.cat),
            bidi_index(&old.bidi),
            eaw_index(&old_eaw),
            old.mirrored as u32,
            if old.decimal.is_empty() { 0xFF } else { old.decimal.parse().unwrap() },
            old.numeric.clone(),
        );
        let id = *map.entry(rec.clone()).or_insert_with(|| {
            recs.push(rec);
            (recs.len() - 1) as u32
        });
        changes.push((cp, id));
    }
    eprintln!("3.2.0: {} assigned ranges, {} changed chars, {} change records, other diffs {:?}",
        ranges.len(), changes.len(), recs.len(), report);

    let mut o = Out::new("#![allow(clippy::all)]\nuse super::super::ChangeRecord;\n\n");
    o.bytes += ranges.len() * 8;
    writeln!(o.s, "pub(crate) static ASSIGNED: [(u32, u32); {}] = [", ranges.len()).unwrap();
    let mut line = String::new();
    for (a, b) in &ranges {
        write!(line, "(0x{a:X},0x{b:X}),").unwrap();
        if line.len() > 90 {
            o.s.push_str(&line);
            o.s.push('\n');
            line.clear();
        }
    }
    o.s.push_str(&line);
    o.s.push_str("\n];\n");
    o.bytes += changes.len() * 8;
    writeln!(o.s, "pub(crate) static CHANGES: [(u32, u16); {}] = [", changes.len()).unwrap();
    let mut line = String::new();
    for (a, b) in &changes {
        write!(line, "(0x{a:X},{b}),").unwrap();
        if line.len() > 90 {
            o.s.push_str(&line);
            o.s.push('\n');
            line.clear();
        }
    }
    o.s.push_str(&line);
    o.s.push_str("\n];\n");
    writeln!(o.s, "pub(crate) static CHANGE_RECORDS: [ChangeRecord; {}] = [", recs.len()).unwrap();
    for r in &recs {
        let num = if r.5.is_empty() { "f64::NAN".to_string() } else { format!("{:?}", parse_numeric(&r.5)) };
        writeln!(o.s, "ChangeRecord{{category:{},bidi:{},eaw:{},mirrored:{},decimal:{},numeric:{}}},",
            r.0, r.1, r.2, r.3 != 0, r.4, num).unwrap();
    }
    o.s.push_str("];\n");
    writeln!(o.s, "pub(crate) static NORMALIZATION_CORRECTIONS: [(u32, u32); {}] = [", corrections.len()).unwrap();
    for (cp, v) in &corrections {
        assert_eq!(v.len(), 1);
        writeln!(o.s, "(0x{cp:X}, 0x{:X}),", v[0]).unwrap();
    }
    o.s.push_str("];\n");
    o.write(&outdir.join("v320.rs"));
}
