//! Loading C extension modules (.so) — pending (phase 4).
use crate::errors::*;
use crate::raise;
pub fn find_extension(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let _ = (dir, name);
    None
}
pub fn load_extension(name: &str, path: &std::path::Path) -> PyResult {
    let _ = path;
    raise!(ImportError, "extension modules not supported yet: {}", name);
}
