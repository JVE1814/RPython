//! Frozen modules: Python modules whose source text is built into the executable.
//!
//! The three tables mirror the groups CPython exports (`_PyImport_FrozenBootstrap`,
//! `_PyImport_FrozenStdlib`, `_PyImport_FrozenTest`); each C table is an array of
//! the public 24-byte `struct _frozen` ending with a NULL name. Our "code" bytes
//! are the module's source; `_imp.get_frozen_object` builds the code object from the
//! start-up image (`startup_image`: compiled and translated at build time) and only
//! compiles the source when the image has no record for it.
//!
//! Whether the frozen copies are used for normal imports is decided by
//! `-X frozen_modules=on|off` / `PYTHON_FROZEN_MODULES` (default, as in CPython: on for
//! an installation, off when the standard library is the source tree's Lib/, where the
//! files on disk are authoritative), and by `_imp._override_frozen_modules_for_tests`.
//! The importlib bootstrap modules are always reported as frozen: they are loaded before
//! any finder exists.

use crate::capi::import::Frozen;
use std::ffi::{CStr, c_char, c_int};
use std::sync::atomic::{AtomicI32, Ordering::*};

pub struct Entry {
    pub name: &'static CStr,
    /// Module whose source this is (aliases), `<pkg` for a package's `__init__`
    /// listed under its own name, None if there is no stdlib file behind it.
    pub orig: Option<&'static str>,
    pub src: &'static [u8],
    /// (perf6) Source path below Lib/ ("" if none): the key of its start-up image record.
    pub file: &'static str,
    pub is_package: bool,
    /// Available even when frozen modules are switched off.
    pub essential: bool,
}

impl Entry {
    pub fn name(&self) -> &'static str {
        self.name.to_str().unwrap_or("")
    }
}

// (mem1) The sources live in the cold read-only section: a start runs the start-up image
// and reads a source only when the image has no record for it (or for `_imp`'s
// `get_frozen_object(..., data=True)`).
macro_rules! lib {
    ($p:literal) => {
        crate::cold_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Lib/", $p))
    };
}

macro_rules! entry {
    ($name:literal, $orig:expr, lib!($p:literal), $pkg:expr) => {
        Entry { name: $name, orig: $orig, src: lib!($p), file: $p, is_package: $pkg, essential: false }
    };
    ($name:literal, $orig:expr, lib!($p:literal), $pkg:expr, essential) => {
        Entry { name: $name, orig: $orig, src: lib!($p), file: $p, is_package: $pkg, essential: true }
    };
    ($name:literal, $orig:expr, $src:expr, $pkg:expr) => {
        Entry { name: $name, orig: $orig, src: $src, file: "", is_package: $pkg, essential: false }
    };
}

/// Stand-in for a frozen module that has no file in the stdlib.
const HELLO_ONLY: &[u8] = b"initialized = True\n\ndef main():\n    print('Hello world!')\n";

static BOOTSTRAP: [Entry; 3] = [
    entry!(c"_frozen_importlib", Some("importlib._bootstrap"), lib!("importlib/_bootstrap.py"), false, essential),
    entry!(c"_frozen_importlib_external", Some("importlib._bootstrap_external"), lib!("importlib/_bootstrap_external.py"), false, essential),
    entry!(c"zipimport", Some("zipimport"), lib!("zipimport.py"), false),
];

static STDLIB: [Entry; 15] = [
    entry!(c"abc", Some("abc"), lib!("abc.py"), false),
    entry!(c"codecs", Some("codecs"), lib!("codecs.py"), false),
    entry!(c"io", Some("io"), lib!("io.py"), false),
    entry!(c"_collections_abc", Some("_collections_abc"), lib!("_collections_abc.py"), false),
    entry!(c"_sitebuiltins", Some("_sitebuiltins"), lib!("_sitebuiltins.py"), false),
    entry!(c"genericpath", Some("genericpath"), lib!("genericpath.py"), false),
    entry!(c"ntpath", Some("ntpath"), lib!("ntpath.py"), false),
    entry!(c"posixpath", Some("posixpath"), lib!("posixpath.py"), false),
    entry!(c"os.path", Some("posixpath"), lib!("posixpath.py"), false),
    entry!(c"os", Some("os"), lib!("os.py"), false),
    entry!(c"site", Some("site"), lib!("site.py"), false),
    entry!(c"stat", Some("stat"), lib!("stat.py"), false),
    entry!(c"importlib.util", Some("importlib.util"), lib!("importlib/util.py"), false),
    entry!(c"importlib.machinery", Some("importlib.machinery"), lib!("importlib/machinery.py"), false),
    entry!(c"runpy", Some("runpy"), lib!("runpy.py"), false),
];

static TEST: [Entry; 11] = [
    entry!(c"__hello__", Some("__hello__"), lib!("__hello__.py"), false),
    entry!(c"__hello_alias__", Some("__hello__"), lib!("__hello__.py"), false),
    entry!(c"__phello_alias__", Some("__hello__"), lib!("__hello__.py"), true),
    entry!(c"__phello_alias__.spam", Some("__hello__"), lib!("__hello__.py"), false),
    entry!(c"__phello__", Some("__phello__"), lib!("__phello__/__init__.py"), true),
    entry!(c"__phello__.__init__", Some("<__phello__"), lib!("__phello__/__init__.py"), false),
    entry!(c"__phello__.ham", Some("__phello__.ham"), lib!("__phello__/ham/__init__.py"), true),
    entry!(c"__phello__.ham.__init__", Some("<__phello__.ham"), lib!("__phello__/ham/__init__.py"), false),
    entry!(c"__phello__.ham.eggs", Some("__phello__.ham.eggs"), lib!("__phello__/ham/eggs.py"), false),
    entry!(c"__phello__.spam", Some("__phello__.spam"), lib!("__phello__/spam.py"), false),
    entry!(c"__hello_only__", None, HELLO_ONLY, false),
];

// ---------------------------------------------------------------- C tables

#[repr(C)]
pub struct CTable<const N: usize> {
    entries: [Frozen; N],
    end: Frozen,
}
unsafe impl<const N: usize> Sync for CTable<N> {}

const fn c_table<const N: usize>(t: &[Entry; N]) -> CTable<N> {
    const END: Frozen = Frozen { name: std::ptr::null(), code: std::ptr::null(), size: 0, is_package: 0 };
    let mut entries = [END; N];
    let mut i = 0;
    while i < N {
        entries[i] = Frozen {
            name: t[i].name.as_ptr() as *const c_char,
            code: t[i].src.as_ptr(),
            size: t[i].src.len() as c_int,
            is_package: t[i].is_package as c_int,
        };
        i += 1;
    }
    CTable { entries, end: END }
}

static C_BOOTSTRAP: CTable<3> = c_table(&BOOTSTRAP);
static C_STDLIB: CTable<15> = c_table(&STDLIB);
static C_TEST: CTable<11> = c_table(&TEST);

#[unsafe(no_mangle)]
pub static mut _PyImport_FrozenBootstrap: *const Frozen = &raw const C_BOOTSTRAP.entries as *const Frozen;
#[unsafe(no_mangle)]
pub static mut _PyImport_FrozenStdlib: *const Frozen = &raw const C_STDLIB.entries as *const Frozen;
#[unsafe(no_mangle)]
pub static mut _PyImport_FrozenTest: *const Frozen = &raw const C_TEST.entries as *const Frozen;

// ---------------------------------------------------------------- policy

/// 1: use frozen modules, -1: do not, 0: follow the configuration.
static OVERRIDE: AtomicI32 = AtomicI32::new(0);
/// Configuration (-X frozen_modules / PYTHON_FROZEN_MODULES): 1 on, -1 off, 0 unknown yet.
static CONFIGURED: AtomicI32 = AtomicI32::new(0);

pub fn set_override(v: i32) {
    OVERRIDE.store(v, Relaxed);
}

fn configured() -> bool {
    let c = CONFIGURED.load(Relaxed);
    if c != 0 {
        return c > 0;
    }
    let cfg = crate::modules::sys_full::config();
    // (perf6) default like CPython: on for an installation, off when the standard library
    // is the source tree (there the files on disk are authoritative)
    let mut on = !crate::modules::sys_full::paths().source_tree;
    if cfg.ignore_environment == 0 {
        if let Ok(v) = std::env::var("PYTHON_FROZEN_MODULES") {
            on = v == "on";
        }
    }
    for x in &cfg.xoptions {
        if x == "frozen_modules" {
            on = true;
        } else if let Some(v) = x.strip_prefix("frozen_modules=") {
            on = v != "off";
        }
    }
    CONFIGURED.store(if on { 1 } else { -1 }, Relaxed);
    on
}

/// Are the frozen copies of non-essential modules in use?
pub fn use_frozen() -> bool {
    match OVERRIDE.load(Relaxed) {
        o if o > 0 => true,
        o if o < 0 => false,
        _ => configured(),
    }
}

fn all() -> impl Iterator<Item = &'static Entry> {
    BOOTSTRAP.iter().chain(STDLIB.iter()).chain(TEST.iter())
}

/// The entry for `name` if it is available under the current policy
/// (`any`: regardless of the policy).
pub fn lookup(name: &str, any: bool) -> Option<&'static Entry> {
    let on = any || use_frozen();
    all().find(|e| e.name.to_bytes() == name.as_bytes() && (on || e.essential))
}

/// Names of the available frozen modules, in table order.
pub fn names() -> Vec<&'static str> {
    let on = use_frozen();
    all().filter(|e| on || e.essential).map(|e| e.name()).collect()
}

/// The stdlib file a frozen module's source corresponds to (used for caching).
pub fn stdlib_file(e: &Entry) -> Option<String> {
    let orig = e.orig?;
    let (rel, pkg) = match orig.strip_prefix('<') {
        Some(p) => (p, true),
        None => (orig, e.is_package && orig == e.name()),
    };
    let base = format!("{}/{}", crate::modules::sys_full::stdlib_dir(), rel.replace('.', "/"));
    Some(if pkg { format!("{}/__init__.py", base) } else { format!("{}.py", base) })
}
