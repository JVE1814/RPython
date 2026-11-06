// Link-time code layout (perf2 workstream): the functions that run during interpreter
// startup are placed next to each other in front of the rest of the code, so
// `python3 -c pass` faults in a compact range of code pages instead of touching most
// of the executable (lower RSS, fewer page faults and iTLB misses). `startup.ld` is a
// linker-script fragment listing their sections; regenerate it with
// tools/bench/perf/gen_order.sh. Names that no longer exist simply match nothing.
// Set RPY_NO_ORDER=1 to link without it.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let script = format!("{}/startup.ld", dir);
    println!("cargo:rerun-if-changed=startup.ld");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RPY_NO_ORDER");
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if linux && std::path::Path::new(&script).exists() && std::env::var_os("RPY_NO_ORDER").is_none() {
        println!("cargo:rustc-link-arg-bins=-Wl,-T,{}", script);
    }
    if linux {
        // The static type objects and method tables hold ~15k pointers that need
        // relative relocations; the compact RELR encoding (glibc >= 2.36) shrinks
        // .rela.dyn from ~370 KB, which the loader reads at every start, to a few KB.
        println!("cargo:rustc-link-arg-bins=-Wl,-z,pack-relative-relocs");
    }
}
