//! Derives a bytecode format number from the compiler (and parser) sources, so
//! that cached .pyc files are invalidated automatically whenever code generation
//! may have changed (the runtime builds its pyc magic number from it).

use std::path::Path;

fn hash_dir(dir: &Path, h: &mut u64) {
    let mut entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return,
    };
    entries.sort();
    for p in entries {
        if p.is_dir() {
            hash_dir(&p, h);
        } else if p.extension().is_some_and(|e| e == "rs") {
            if let Ok(data) = std::fs::read(&p) {
                for b in data {
                    *h = (*h ^ b as u64).wrapping_mul(0x100_0000_01b3);
                }
            }
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for d in [root.join("src"), root.join("../parser/src")] {
        hash_dir(&d, &mut h);
        println!("cargo:rerun-if-changed={}", d.display());
    }
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(
        Path::new(&out).join("bytecode_hash.rs"),
        format!("/// Hash of the compiler/parser sources (see build.rs).\npub const BYTECODE_HASH: u32 = {};\n", (h ^ (h >> 32)) as u32),
    )
    .unwrap();
}
